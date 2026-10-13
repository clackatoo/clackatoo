//! Talking to Chromium over CDP, the protocol of Chrome's developer
//! tools, on the pipe it opens with `--remote-debugging-pipe`: it reads
//! commands on
//! its fd 3 and writes answers and events on its fd 4, each message JSON
//! ending in a NUL byte.
//!
//! A pipe, rather than `--remote-debugging-port`, because a debugging
//! port is a TCP socket on localhost that anyone on the machine could
//! connect to and drive the browser through, with no password. Only the
//! daemon holds the pipe's ends. And the browser stops by itself when the
//! pipe closes, as when the daemon goes.
//!
//! Commands are sent from the browser's thread, which waits for each
//! answer. A thread of the connection's own reads what the browser
//! writes: answers go to the command waiting for them, and events to the
//! browser's thread, among its jobs.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, PipeReader, PipeWriter, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use super::{Input, Running};

/// How long a command waits for its answer, unless it says otherwise.
pub(super) const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// How long the browser has to start and answer its first command.
const START_WAIT: Duration = Duration::from_secs(20);

/// The most a message from the browser may be, in bytes: a picture of a
/// big page, as base64, is a few megabytes.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// The arguments Chromium is run with, beside its profile and the pipe.
/// Headless, with nothing of its own going on in the background: no
/// first-run pages, sync, extensions, or keyring, which on a desktop asks
/// the user to unlock it.
const ARGS: &[&str] = &[
    "--headless=new",
    "--remote-debugging-pipe",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-extensions",
    "--disable-sync",
    "--disable-background-networking",
    "--disable-component-update",
    "--password-store=basic",
    "--mute-audio",
    "--window-size=1280,800",
    "about:blank",
];

/// What the browser answered a command: its result, or why it failed.
pub(super) type Answer = Result<Value, String>;

/// A running Chromium, and the pipe to it.
#[derive(Debug)]
pub(super) struct Cdp {
    child: Child,
    writer: Arc<Mutex<PipeWriter>>,
    /// The number the next command gets, shared with the reader, which
    /// answers dialogs itself.
    next_id: Arc<AtomicU64>,
    /// The commands waiting for answers, by number.
    waiting: Arc<Mutex<HashMap<u64, mpsc::Sender<Answer>>>>,
}

impl Cdp {
    /// Runs `program` with `profile` as its profile directory, and talks
    /// to it, sending its events to `inputs`, and saying it's gone as
    /// `launch`. Its process goes in `running` as soon as it's started. Checks the browser uses that
    /// profile, as a confined one (a snap) silently falls back on the
    /// user's own if it can't reach it, and refuses it if not.
    pub(super) fn launch(
        program: &Path,
        profile: &Path,
        (launch, inputs): (u64, mpsc::Sender<Input>),
        running: &Running,
    ) -> Result<Self, String> {
        fs::create_dir_all(profile)
            .map_err(|err| format!("{} couldn't be made: {err}", profile.display()))?;
        let failed = |err: io::Error| format!("{} couldn't be started: {err}", program.display());
        let (browser_reads, commands) = io::pipe().map_err(failed)?;
        let (events, browser_writes) = io::pipe().map_err(failed)?;
        let child = spawn(program, profile, browser_reads, browser_writes).map_err(failed)?;
        // From now on, the daemon can stop it, even as it starts.
        let pid = rustix::process::Pid::from_child(&child);
        running.set(rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok());
        let mut cdp = Self {
            child,
            writer: Arc::new(Mutex::new(commands)),
            next_id: Arc::new(AtomicU64::new(1)),
            waiting: Arc::new(Mutex::new(HashMap::new())),
        };
        let reader = Reader {
            launch,
            writer: Arc::clone(&cdp.writer),
            next_id: Arc::clone(&cdp.next_id),
            waiting: Arc::clone(&cdp.waiting),
            inputs,
        };
        std::thread::Builder::new()
            .name("browser pipe".to_owned())
            .spawn(move || reader.run(events))
            .map_err(failed)?;
        if let Err(why) = cdp.call(None, "Browser.getVersion", json!({}), START_WAIT) {
            cdp.stop();
            return Err(format!(
                "{} started, but didn't answer: {why}",
                program.display()
            ));
        }
        if !uses(profile, cdp.child.id()) {
            cdp.stop();
            return Err(format!(
                "{} didn't use the profile clackatoo gave it, {}, and may have fallen back on \
                 yours, so it was stopped. A snap can't reach hidden folders: set \
                 CLACKATOO_BROWSER_PROFILE to a folder it can, such as ~/snap/chromium/common/clackatoo",
                program.display(),
                profile.display()
            ));
        }
        Ok(cdp)
    }

    /// Sends the command `method` with `params`, to the page attached as
    /// `session`, or to the browser with `None`, and waits up to `wait`
    /// for its answer.
    pub(super) fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Answer {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = mpsc::channel();
        (self.waiting.lock().unwrap_or_else(PoisonError::into_inner)).insert(id, answer);
        let mut message = json!({ "id": id, "method": method });
        message["params"] = params;
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        if let Err(err) = send(&self.writer, &message) {
            self.forget(id);
            return Err(format!("the browser's pipe broke: {err}"));
        }
        match answered.recv_timeout(wait) {
            Ok(answer) => answer,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.forget(id);
                Err(format!(
                    "the browser didn't answer {method} within {} s",
                    wait.as_secs()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("the browser has stopped".to_owned()),
        }
    }

    /// Sends the command `method` without waiting for its answer, which
    /// is dropped.
    pub(super) fn tell(&mut self, session: Option<&str>, method: &str, params: Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method });
        message["params"] = params;
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        if let Err(err) = send(&self.writer, &message) {
            log::debug!("the browser's pipe broke: {err}");
        }
    }

    fn forget(&self, id: u64) {
        (self.waiting.lock().unwrap_or_else(PoisonError::into_inner)).remove(&id);
    }

    /// Whether the browser is still running.
    pub(super) fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Closes the browser, asking first, and waits for it to go.
    pub(super) fn stop(&mut self) {
        if self.running() {
            let closed = self.call(None, "Browser.close", json!({}), Duration::from_secs(2));
            if closed.is_err() || !wait_for_exit(&mut self.child, Duration::from_secs(3)) {
                // It stops when its pipe closes too, but a hung one may not.
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}

impl Drop for Cdp {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Waits up to `wait` for `child` to exit. Returns whether it did.
fn wait_for_exit(child: &mut Child, wait: Duration) -> bool {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) if std::time::Instant::now() >= deadline => return false,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Whether the browser with process ID `pid` holds `profile`: Chromium
/// links `SingletonLock` there to `HOST-PID` while it uses it.
fn uses(profile: &Path, pid: u32) -> bool {
    fs::read_link(profile.join("SingletonLock"))
        .ok()
        .and_then(|target| target.to_str().map(str::to_owned))
        .is_some_and(|target| target.rsplit_once('-').map(|(_, pid)| pid) == Some(&pid.to_string()))
}

/// Writes `message` to the browser, ending it with a NUL.
fn send(writer: &Mutex<PipeWriter>, message: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(message).map_err(io::Error::other)?;
    bytes.push(0);
    let mut writer = writer.lock().unwrap_or_else(PoisonError::into_inner);
    writer.write_all(&bytes)
}

/// Starts `program` with `browser_reads` as its fd 3 and `browser_writes`
/// as its fd 4, where `--remote-debugging-pipe` looks for them.
#[allow(unsafe_code)]
fn spawn(
    program: &Path,
    profile: &Path,
    browser_reads: PipeReader,
    browser_writes: PipeWriter,
) -> io::Result<Child> {
    // Out of the way of 3 and 4 first, so putting one there can't close
    // the other. Both are close-on-exec, so only the copies at 3 and 4
    // reach the browser.
    let reads = rustix::io::fcntl_dupfd_cloexec(browser_reads.as_fd(), 10)?;
    let writes = rustix::io::fcntl_dupfd_cloexec(browser_writes.as_fd(), 10)?;
    drop((browser_reads, browser_writes));
    let mut command = Command::new(program);
    command
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure runs in the child between fork and exec, where
    // only async-signal-safe operations are allowed. It makes two dup2
    // system calls through rustix, with no allocation, locking or shared
    // state. The fds 3 and 4 it writes over are wrapped in `ManuallyDrop`,
    // so nothing closes them; dup2 leaves the copies open across exec.
    unsafe {
        command.pre_exec(move || {
            let mut three = ManuallyDrop::new(OwnedFd::from_raw_fd(3));
            let mut four = ManuallyDrop::new(OwnedFd::from_raw_fd(4));
            rustix::io::dup2(&reads, &mut three)?;
            rustix::io::dup2(&writes, &mut four)?;
            Ok(())
        });
    }
    command.spawn()
}

/// What reads the browser's pipe, on a thread of its own.
struct Reader {
    /// The browser's number, as its thread counts them.
    launch: u64,
    writer: Arc<Mutex<PipeWriter>>,
    next_id: Arc<AtomicU64>,
    waiting: Arc<Mutex<HashMap<u64, mpsc::Sender<Answer>>>>,
    inputs: mpsc::Sender<Input>,
}

impl Reader {
    /// Reads messages until the browser closes the pipe, then says so.
    fn run(self, events: PipeReader) {
        let mut events = BufReader::with_capacity(256 * 1024, events);
        let mut message = Vec::new();
        loop {
            message.clear();
            match events
                .by_ref()
                .take(MAX_MESSAGE as u64)
                .read_until(0, &mut message)
            {
                Ok(0) | Err(_) => break,
                Ok(_) if message.last() != Some(&0) => {
                    log::warn!("the browser sent a message too big or cut off");
                    break;
                }
                Ok(_) => {
                    message.pop();
                    match serde_json::from_slice::<Value>(&message) {
                        Ok(message) => self.receive(message),
                        Err(err) => log::warn!("the browser sent something not JSON: {err}"),
                    }
                }
            }
        }
        // Commands still waiting are told it's gone, by their senders
        // going.
        (self.waiting.lock().unwrap_or_else(PoisonError::into_inner)).clear();
        let _ = self.inputs.send(Input::Gone(self.launch));
    }

    fn receive(&self, mut message: Value) {
        if let Some(id) = message["id"].as_u64() {
            let waiting = (self.waiting.lock().unwrap_or_else(PoisonError::into_inner)).remove(&id);
            if let Some(waiting) = waiting {
                let answer = match message.get_mut("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("failed").to_owned()),
                    None => Ok(message["result"].take()),
                };
                let _ = waiting.send(answer);
            }
            return;
        }
        let Some(method) = message["method"].as_str().map(str::to_owned) else {
            return;
        };
        let session = message["sessionId"].as_str().map(str::to_owned);
        if method == "Page.javascriptDialogOpening" {
            // An alert, a confirm or a prompt stops the page, and the
            // command that opened it, until it's answered, so it's
            // answered here rather than behind that command: turned down,
            // but a page asking whether to leave is let go.
            let leaving = message["params"]["type"] == "beforeunload";
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let mut answer = json!({
                "id": id,
                "method": "Page.handleJavaScriptDialog",
                "params": { "accept": leaving },
            });
            if let Some(session) = &session {
                answer["sessionId"] = json!(session);
            }
            if let Err(err) = send(&self.writer, &answer) {
                log::debug!("answering a dialog failed: {err}");
            }
        }
        let params = message["params"].take();
        let _ = self.inputs.send(Input::Event {
            method,
            params,
            session,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_is_the_browsers_only_if_its_lock_names_it() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!uses(dir.path(), 4242), "no lock: someone else's profile");
        std::os::unix::fs::symlink("desktop-4242", dir.path().join("SingletonLock")).unwrap();
        assert!(uses(dir.path(), 4242));
        assert!(!uses(dir.path(), 424), "another browser's");
        assert!(!uses(dir.path(), 42_420));
    }
}
