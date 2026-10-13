//! clackatoo's browser: a headless Chromium the daemon runs for agents to
//! drive, with a page for each chat whose Browser setting is on, and
//! pictures of each page for the panes watching it.
//!
//! The browser is the daemon's own, with a profile of its own: never the
//! user's browser, nor their profile. Each chat's page is in a browser
//! context of its own, as a private window is, so chats share no cookies
//! or logins, and nothing a page stores outlasts the chat.
//!
//! It starts with the first page an agent opens, and stops a while after
//! the last chat using it lets it go. It doesn't outlast the daemon, nor
//! an upgrade: the next page starts it again.
//!
//! Everything here runs on a thread of the browser's own, as talking to
//! Chromium means waiting for it ([`cdp`]). The daemon hands it [`Job`]s,
//! and it puts what comes of them in an [`Outbox`], then wakes the event
//! loop. While a window watches a page, Chromium sends a picture each time
//! the page changes; the thread holds back its acknowledgement of each,
//! which Chromium waits for before sending the next, so pictures come a
//! few a second at most however busy the page is.

mod cdp;
mod snapshot;

use std::collections::{BTreeMap, VecDeque};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine as _;
use clackatoo_protocol::{BrowserStatus, TerminalId};
use serde_json::{Value, json};

use cdp::Cdp;

/// The least time between two pictures of a page: four a second.
const FRAME_GAP: Duration = Duration::from_millis(250);

/// How long opening a page waits for it to load.
const LOAD_WAIT: Duration = Duration::from_secs(20);

/// How long after a click or a key the page is watched for starting to
/// load another, and how long that's then waited for.
const SETTLE_WAIT: Duration = Duration::from_millis(300);
const SETTLE_LOAD_WAIT: Duration = Duration::from_secs(15);

/// The most of a page's outline an agent is given, in bytes.
const MAX_OUTLINE: usize = 40 * 1024;

/// The most of a page's text an agent is given, in bytes.
const MAX_TEXT: usize = 32 * 1024;

/// How big a page is: as a laptop's browser window is.
const PAGE_SIZE: [u32; 2] = [1280, 800];

/// The browsers looked for on `PATH`, in order. One that isn't a snap is
/// taken first, as a snap can't reach the folder profiles go in.
const PROGRAMS: &[&str] = &[
    "google-chrome-stable",
    "google-chrome",
    "chromium-browser",
    "chromium",
];

/// Where the browser is, the profile it runs with, and how long it's kept
/// with no pages.
#[derive(Clone, Debug)]
pub struct BrowserConfig {
    /// The browser: Chromium or Chrome. `None` looks for one on `PATH`.
    pub program: Option<PathBuf>,
    /// The profile directory it runs with. `None` puts it in
    /// `~/snap/NAME/common/clackatoo-browser` for a snap, which can reach
    /// nothing hidden in the home directory, or else in `data`.
    pub profile: Option<PathBuf>,
    /// Where the profile goes for a browser that isn't a snap, unless
    /// `profile` says.
    pub data: Option<PathBuf>,
    /// The home directory, which snaps keep their folders in.
    pub home: PathBuf,
    /// How long the browser keeps running once no chat has a page.
    pub idle: Duration,
}

/// Something for the browser's thread to do.
#[derive(Debug)]
pub(crate) enum Job {
    /// Do what an agent asked, for its chat, answering as call `call`.
    Act {
        call: u64,
        chat: TerminalId,
        act: Act,
    },
    /// Send pictures of the chat's page, at most this big, or with `None`,
    /// stop.
    Watch {
        chat: TerminalId,
        size: Option<[u32; 2]>,
    },
    /// Close the chat's page, as the chat closed or its Browser setting
    /// went off.
    Close(TerminalId),
    /// Stop the browser, and the thread.
    Stop,
}

/// What an agent asked of its page.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// Open an address.
    Navigate { url: String },
    /// Go back a page.
    Back,
    /// Read the page's outline.
    Snapshot,
    /// Click the thing with this ref.
    Click { target: u64 },
    /// Type into the thing with this ref, in place of what it holds, then
    /// press Enter if `submit`.
    Type {
        target: u64,
        text: String,
        submit: bool,
    },
    /// Press a key, in whatever has the focus.
    Press { key: String },
    /// Take a picture of the page.
    Screenshot,
    /// Read the page's text.
    Read,
}

/// What an act gives the agent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// Words.
    Text(String),
    /// Words, and a picture of the page, a PNG as base64.
    Picture { text: String, png: String },
}

/// What the browser's thread has for the daemon.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    /// What came of acts, by call.
    pub(crate) answers: Vec<(u64, Result<Answer, String>)>,
    /// What chats' pages are now, if that changed.
    pub(crate) statuses: BTreeMap<TerminalId, BrowserStatus>,
    /// The latest picture of each watched page, as a JPEG, if there's a
    /// new one.
    pub(crate) frames: BTreeMap<TerminalId, Vec<u8>>,
}

/// What the browser's thread hears: jobs from the daemon, and what the
/// browser says.
#[derive(Debug)]
enum Input {
    Job(Job),
    /// An event from the browser: its method, its parameters, and the
    /// page's session it's about, if it's about one.
    Event {
        method: String,
        params: Value,
        session: Option<String>,
    },
    /// The browser started as this number closed its pipe: it has
    /// stopped.
    Gone(u64),
}

/// The daemon's side of the browser's thread.
#[derive(Debug)]
pub(crate) struct Browser {
    inputs: mpsc::Sender<Input>,
    outbox: Arc<Mutex<Outbox>>,
    /// The running browser's process, as a pidfd, to stop it by without
    /// waiting for the thread, which may be waiting for a page to load.
    running: Arc<Running>,
    thread: Option<JoinHandle<()>>,
}

/// The running browser's process, if one is, shared between the thread
/// and the daemon, and whether the daemon is stopping it.
#[derive(Debug, Default)]
struct Running {
    process: Mutex<Option<OwnedFd>>,
    stopping: AtomicBool,
}

impl Running {
    /// The browser's process is now `process`, as a pidfd, or none.
    fn set(&self, process: Option<OwnedFd>) {
        *self.process.lock().unwrap_or_else(PoisonError::into_inner) = process;
    }

    /// Whether the daemon is stopping the browser.
    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// Stops the browser, if one is running, and any that's starting once
    /// it has.
    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let process = self.process.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(process) = process.as_ref() {
            let _ = rustix::process::pidfd_send_signal(process, rustix::process::Signal::TERM);
        }
    }
}

impl Browser {
    /// Starts the browser's thread, which starts the browser when it's
    /// first needed, and calls `wake` whenever it has something in its
    /// outbox.
    pub(crate) fn start(config: BrowserConfig, wake: impl Fn() + Send + 'static) -> Self {
        let (inputs, heard) = mpsc::channel();
        let outbox = Arc::new(Mutex::new(Outbox::default()));
        let running = Arc::new(Running::default());
        let worker = Worker {
            running: Arc::clone(&running),
            config,
            heard,
            inputs: inputs.clone(),
            outbox: Arc::clone(&outbox),
            wake: Box::new(wake),
            cdp: None,
            launches: 0,
            pages: BTreeMap::new(),
            watching: BTreeMap::new(),
            deferred: VecDeque::new(),
            idle_since: None,
        };
        let thread = std::thread::Builder::new()
            .name("browser".to_owned())
            .spawn(move || worker.run())
            .map_err(|err| log::error!("the browser's thread couldn't start: {err}"))
            .ok();
        Self {
            inputs,
            outbox,
            running,
            thread,
        }
    }

    /// Hands the thread `job`.
    pub(crate) fn send(&self, job: Job) {
        if self.inputs.send(Input::Job(job)).is_err() {
            log::warn!("the browser's thread has gone");
        }
    }

    /// Takes everything in the outbox.
    pub(crate) fn take(&self) -> Outbox {
        std::mem::take(&mut *locked(&self.outbox))
    }

    /// Stops the browser and its thread, waiting for them. The browser is
    /// told to stop at once, so whatever the thread waits on fails, rather
    /// than the daemon waiting for a page to load.
    pub(crate) fn stop(&mut self) {
        let _ = self.inputs.send(Input::Job(Job::Stop));
        self.running.stop();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("the browser's thread panicked");
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A chat's page.
#[derive(Debug)]
struct Page {
    /// Its browser context, which only it is in.
    context: String,
    /// Its target: the page itself, whose ID is its main frame's too.
    target: String,
    /// The session the thread is attached to it by.
    session: String,
    status: BrowserStatus,
    /// The size pictures of it are being sent at, if they are.
    casting: Option<[u32; 2]>,
    /// The picture to acknowledge, by Chromium's number for it, and when.
    ack: Option<(Instant, Value)>,
    /// When the last picture was acknowledged.
    acked: Option<Instant>,
    /// Dialogs turned down since the agent last heard, said as it's told.
    notes: Vec<String>,
}

/// The browser's thread.
struct Worker {
    config: BrowserConfig,
    /// The running browser's process, for the daemon to stop it by.
    running: Arc<Running>,
    heard: mpsc::Receiver<Input>,
    /// For the browser's pipe to send its events with.
    inputs: mpsc::Sender<Input>,
    outbox: Arc<Mutex<Outbox>>,
    wake: Box<dyn Fn() + Send>,
    cdp: Option<Cdp>,
    /// How many times the browser has been started: the number of the
    /// one running, if one is.
    launches: u64,
    pages: BTreeMap<TerminalId, Page>,
    /// The size each chat's page is watched at, by the panes showing it,
    /// whether or not it has a page yet.
    watching: BTreeMap<TerminalId, [u32; 2]>,
    /// Jobs that came while an act waited for its page, done after it.
    deferred: VecDeque<Job>,
    /// When the last page closed, while the browser runs with none.
    idle_since: Option<Instant>,
}

impl Worker {
    fn run(mut self) {
        loop {
            let input = if let Some(job) = self.deferred.pop_front() {
                Input::Job(job)
            } else {
                let heard = match self.next_due() {
                    Some(due) => match self.heard.recv_timeout(due - Instant::now().min(due)) {
                        Ok(input) => Some(input),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                    None => match self.heard.recv() {
                        Ok(input) => Some(input),
                        Err(mpsc::RecvError) => break,
                    },
                };
                let Some(input) = heard else {
                    self.chores();
                    continue;
                };
                input
            };
            match input {
                Input::Job(Job::Stop) => break,
                Input::Job(job) => self.job(job),
                Input::Event {
                    method,
                    params,
                    session,
                } => self.event(&method, &params, session.as_deref()),
                Input::Gone(launch) => self.gone(launch),
            }
            self.chores();
        }
        let chats: Vec<TerminalId> = self.pages.keys().copied().collect();
        self.pages.clear();
        for chat in chats {
            self.publish_closed(chat, None);
        }
        self.running.set(None);
        if let Some(mut cdp) = self.cdp.take() {
            cdp.stop();
        }
    }

    fn job(&mut self, job: Job) {
        match job {
            Job::Act { call, chat, act } => {
                let answer = self.act(chat, act);
                self.lock().answers.push((call, answer));
                (self.wake)();
            }
            Job::Watch { chat, size } => self.watch(chat, size),
            Job::Close(chat) => self.close(chat),
            Job::Stop => {}
        }
    }

    fn lock(&self) -> MutexGuard<'_, Outbox> {
        locked(&self.outbox)
    }

    // --- Acts -------------------------------------------------------------

    fn act(&mut self, chat: TerminalId, act: Act) -> Result<Answer, String> {
        if let Act::Navigate { url } = act {
            return self.navigate(chat, &url);
        }
        if !self.pages.contains_key(&chat) {
            return Err("no page is open: open one with browser_navigate first".to_owned());
        }
        let answer = match act {
            Act::Navigate { .. } => unreachable!("handled above"),
            Act::Back => self.back(chat),
            Act::Snapshot => self.snapshot(chat),
            Act::Click { target } => self.click(chat, target),
            Act::Type {
                target,
                text,
                submit,
            } => self.type_into(chat, target, &text, submit),
            Act::Press { key } => self.press(chat, &key),
            Act::Screenshot => self.screenshot(chat),
            Act::Read => self.read(chat),
        };
        self.with_notes(chat, answer)
    }

    /// `answer`, with what the agent should hear of the dialogs turned
    /// down meanwhile.
    fn with_notes(
        &mut self,
        chat: TerminalId,
        answer: Result<Answer, String>,
    ) -> Result<Answer, String> {
        let notes = (self.pages.get_mut(&chat))
            .map(|page| std::mem::take(&mut page.notes))
            .unwrap_or_default();
        if notes.is_empty() {
            return answer;
        }
        let notes = notes.join("\n");
        Ok(match answer? {
            Answer::Text(text) => Answer::Text(format!("{text}\n{notes}")),
            Answer::Picture { text, png } => Answer::Picture {
                text: format!("{text}\n{notes}"),
                png,
            },
        })
    }

    fn navigate(&mut self, chat: TerminalId, url: &str) -> Result<Answer, String> {
        let url = checked_url(url)?;
        self.open_page(chat)?;
        let session = self.pages[&chat].session.clone();
        let opened = self.call(&session, "Page.navigate", json!({ "url": url }))?;
        if let Some(why) = opened["errorText"].as_str().filter(|why| !why.is_empty()) {
            self.refresh(chat, None);
            return Err(format!("{url} couldn't be opened: {why}"));
        }
        let loaded = self.wait_for(&session, Instant::now() + LOAD_WAIT, |method, _| {
            method == "Page.loadEventFired"
        });
        self.refresh(chat, Some(format!("Opened {url}")));
        let mut said = self.where_now(chat);
        if !loaded {
            said.push_str("\nIt's still loading.");
        }
        said.push_str("\nbrowser_snapshot shows what's on it.");
        self.with_notes(chat, Ok(Answer::Text(said)))
    }

    fn back(&mut self, chat: TerminalId) -> Result<Answer, String> {
        let session = self.pages[&chat].session.clone();
        let history = self.call(&session, "Page.getNavigationHistory", json!({}))?;
        let current = history["currentIndex"].as_u64().unwrap_or_default();
        let entries = history["entries"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let previous = (current.checked_sub(1))
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| entries.get(index));
        let Some(previous) = previous else {
            return Err("there's no page to go back to".to_owned());
        };
        let entry = previous["id"].clone();
        self.call(
            &session,
            "Page.navigateToHistoryEntry",
            json!({ "entryId": entry }),
        )?;
        let target = self.pages[&chat].target.clone();
        self.wait_for(
            &session,
            Instant::now() + SETTLE_LOAD_WAIT,
            |method, params| {
                method == "Page.loadEventFired"
                    || (method == "Page.frameNavigated"
                        && params["frame"]["id"] == target.as_str()
                        && params["type"] == "BackForwardCacheRestore")
            },
        );
        self.refresh(chat, Some("Went back".to_owned()));
        Ok(Answer::Text(self.where_now(chat)))
    }

    fn snapshot(&mut self, chat: TerminalId) -> Result<Answer, String> {
        let session = self.pages[&chat].session.clone();
        let tree = self.call(&session, "Accessibility.getFullAXTree", json!({}))?;
        let nodes = tree["nodes"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let outline = snapshot::outline(nodes, MAX_OUTLINE);
        self.refresh(chat, None);
        Ok(Answer::Text(format!(
            "{}\n\n{outline}",
            self.where_now(chat)
        )))
    }

    fn click(&mut self, chat: TerminalId, target: u64) -> Result<Answer, String> {
        let session = self.pages[&chat].session.clone();
        let what = self.describe(&session, target)?;
        let node = json!({ "backendNodeId": target });
        // Not every element can be scrolled to; it's clicked where it is.
        let _ = self.call(&session, "DOM.scrollIntoViewIfNeeded", node.clone());
        let quads = self
            .call(&session, "DOM.getContentQuads", node)
            .map_err(|why| {
                format!(
                    "{what} can't be clicked: {why}. It may be hidden: take a new browser_snapshot"
                )
            })?;
        let Some([x, y]) = quads["quads"]
            .as_array()
            .and_then(|quads| quads.first())
            .and_then(centre)
        else {
            return Err(format!("{what} isn't on the page where it can be clicked"));
        };
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            let mut event = json!({ "type": kind, "x": x, "y": y });
            if kind != "mouseMoved" {
                event["button"] = json!("left");
                event["clickCount"] = json!(1);
            }
            self.call(&session, "Input.dispatchMouseEvent", event)?;
        }
        self.settle(chat);
        self.refresh(chat, Some(format!("Clicked {what}")));
        Ok(Answer::Text(format!(
            "Clicked {what}.\n{}",
            self.where_now(chat)
        )))
    }

    fn type_into(
        &mut self,
        chat: TerminalId,
        target: u64,
        text: &str,
        submit: bool,
    ) -> Result<Answer, String> {
        let session = self.pages[&chat].session.clone();
        let what = self.describe(&session, target)?;
        let node = json!({ "backendNodeId": target });
        self.call(&session, "DOM.focus", node.clone())
            .map_err(|why| format!("{what} can't be typed into: {why}"))?;
        // What it holds is selected, so typing replaces it.
        let object = self.call(&session, "DOM.resolveNode", node)?;
        if let Some(object) = object["object"]["objectId"].as_str() {
            let select = "function() { if (typeof this.select === 'function') { this.select(); } \
                else if (this.isContentEditable) { const range = document.createRange(); \
                range.selectNodeContents(this); const selection = getSelection(); \
                selection.removeAllRanges(); selection.addRange(range); } }";
            self.call(
                &session,
                "Runtime.callFunctionOn",
                json!({ "objectId": object, "functionDeclaration": select }),
            )?;
        }
        self.call(&session, "Input.insertText", json!({ "text": text }))?;
        if submit {
            self.key(&session, "Enter")?;
            self.settle(chat);
        }
        let chars = text.chars().count();
        let typed = format!(
            "Typed {chars} character{} into {what}{}",
            if chars == 1 { "" } else { "s" },
            if submit { ", then pressed Enter" } else { "" }
        );
        self.refresh(chat, Some(typed.clone()));
        Ok(Answer::Text(format!("{typed}.\n{}", self.where_now(chat))))
    }

    fn press(&mut self, chat: TerminalId, key: &str) -> Result<Answer, String> {
        let session = self.pages[&chat].session.clone();
        self.key(&session, key)?;
        self.settle(chat);
        self.refresh(chat, Some(format!("Pressed {key}")));
        Ok(Answer::Text(format!(
            "Pressed {key}.\n{}",
            self.where_now(chat)
        )))
    }

    fn screenshot(&mut self, chat: TerminalId) -> Result<Answer, String> {
        let session = self.pages[&chat].session.clone();
        let shot = self.call(
            &session,
            "Page.captureScreenshot",
            json!({ "format": "png" }),
        )?;
        let png = shot["data"]
            .as_str()
            .ok_or("the browser sent no picture")?
            .to_owned();
        self.refresh(chat, None);
        let text = format!("A picture of the page. {}", self.where_now(chat));
        Ok(Answer::Picture { text, png })
    }

    fn read(&mut self, chat: TerminalId) -> Result<Answer, String> {
        let session = self.pages[&chat].session.clone();
        let read = self.call(
            &session,
            "Runtime.evaluate",
            json!({
                "expression": "(document.body || document.documentElement).innerText",
                "returnByValue": true,
            }),
        )?;
        let text = read["result"]["value"].as_str().unwrap_or_default();
        let mut text = text.to_owned();
        if text.len() > MAX_TEXT {
            let mut end = MAX_TEXT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n… (cut off here)");
        }
        self.refresh(chat, None);
        Ok(Answer::Text(format!("{}\n\n{text}", self.where_now(chat))))
    }

    /// What the thing with ref `target` is, as `button "Sign in"`, or why
    /// there's no such thing.
    fn describe(&mut self, session: &str, target: u64) -> Result<String, String> {
        let gone =
            |_| format!("there's no ref {target} on the page now: take a new browser_snapshot");
        let tree = self
            .call(
                session,
                "Accessibility.getPartialAXTree",
                json!({ "backendNodeId": target, "fetchRelatives": false }),
            )
            .map_err(gone)?;
        let node = tree["nodes"].as_array().and_then(|nodes| nodes.first());
        let role = node.and_then(|node| node["role"]["value"].as_str());
        let name = node.and_then(|node| node["name"]["value"].as_str());
        Ok(match (role, name) {
            (Some(role), Some(name)) if !name.trim().is_empty() => {
                let name: String = name.trim().chars().take(60).collect();
                format!("{role} \"{name}\"")
            }
            (Some(role), _) => format!("{role} (ref {target})"),
            _ => format!("ref {target}"),
        })
    }

    /// Presses `key` in the page attached as `session`.
    fn key(&mut self, session: &str, key: &str) -> Result<(), String> {
        let (key, code, text) = key_named(key).ok_or_else(|| {
            format!(
                "{key:?} isn't a key browser_press_key knows: name one such as Enter, Tab, \
                 Escape, Backspace, ArrowDown, or give one character"
            )
        })?;
        let mut down = json!({ "type": "keyDown", "key": key, "windowsVirtualKeyCode": code });
        if let Some(text) = &text {
            down["text"] = json!(text);
        }
        self.call(session, "Input.dispatchKeyEvent", down)?;
        let up = json!({ "type": "keyUp", "key": key, "windowsVirtualKeyCode": code });
        self.call(session, "Input.dispatchKeyEvent", up)?;
        Ok(())
    }

    /// Waits a moment for the chat's page to start loading another, as a
    /// click on a link makes it, and if it does, for that to load.
    fn settle(&mut self, chat: TerminalId) {
        let Some(page) = self.pages.get(&chat) else {
            return;
        };
        let (session, target) = (page.session.clone(), page.target.clone());
        let loading = self.wait_for(&session, Instant::now() + SETTLE_WAIT, |method, params| {
            method == "Page.frameStartedLoading" && params["frameId"] == target.as_str()
        });
        if loading {
            self.wait_for(&session, Instant::now() + SETTLE_LOAD_WAIT, |method, _| {
                method == "Page.loadEventFired"
            });
        }
    }

    /// Where the chat's page is, to tell the agent.
    fn where_now(&self, chat: TerminalId) -> String {
        match self.pages.get(&chat) {
            Some(page) if page.status.title.is_empty() => format!("Page: {}", page.status.url),
            Some(page) => format!("Page: {} \"{}\"", page.status.url, page.status.title),
            None => "No page is open.".to_owned(),
        }
    }

    // --- The browser and its pages ----------------------------------------

    /// Starts the browser, if it isn't running.
    fn start_browser(&mut self) -> Result<(), String> {
        if let Some(cdp) = &mut self.cdp {
            if cdp.running() {
                return Ok(());
            }
            self.stopped();
        }
        let (program, profile) = locate(&self.config)?;
        log::info!(
            "starting the browser: {}, with its profile in {}",
            program.display(),
            profile.display()
        );
        if self.running.stopping() {
            return Err("the daemon is stopping".to_owned());
        }
        self.launches += 1;
        let mut cdp = Cdp::launch(
            &program,
            &profile,
            (self.launches, self.inputs.clone()),
            &self.running,
        )
        .inspect_err(|_| self.running.set(None))?;
        // Stopped as it started: it may not have heard.
        if self.running.stopping() {
            self.running.set(None);
            cdp.stop();
            return Err("the daemon is stopping".to_owned());
        }
        // Nothing an agent clicks is saved to disk.
        let deny = json!({ "behavior": "deny" });
        if let Err(why) = cdp.call(None, "Browser.setDownloadBehavior", deny, cdp::ANSWER_WAIT) {
            log::warn!("the browser wouldn't turn downloads off: {why}");
        }
        self.cdp = Some(cdp);
        Ok(())
    }

    /// Opens a page for the chat, starting the browser, unless it has one.
    fn open_page(&mut self, chat: TerminalId) -> Result<(), String> {
        if self.pages.contains_key(&chat) {
            return Ok(());
        }
        if let Err(why) = self.start_browser() {
            let status = BrowserStatus {
                failure: Some(why.clone()),
                ..BrowserStatus::default()
            };
            self.lock().statuses.insert(chat, status);
            (self.wake)();
            return Err(why);
        }
        let context = self.browser_call("Target.createBrowserContext", json!({}))?;
        let context = context["browserContextId"]
            .as_str()
            .ok_or("the browser made no context")?
            .to_owned();
        let target = self.browser_call(
            "Target.createTarget",
            json!({
                "url": "about:blank",
                "browserContextId": context,
                "width": PAGE_SIZE[0],
                "height": PAGE_SIZE[1],
            }),
        )?;
        let target = target["targetId"]
            .as_str()
            .ok_or("the browser made no page")?
            .to_owned();
        let attached = self.browser_call(
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or("the browser couldn't attach to the page")?
            .to_owned();
        self.call(&session, "Page.enable", json!({}))?;
        self.pages.insert(
            chat,
            Page {
                context,
                target,
                session,
                status: BrowserStatus {
                    running: true,
                    ..BrowserStatus::default()
                },
                casting: None,
                ack: None,
                acked: None,
                notes: Vec::new(),
            },
        );
        self.idle_since = None;
        if let Some(&size) = self.watching.get(&chat) {
            self.cast(chat, Some(size));
        }
        Ok(())
    }

    fn close(&mut self, chat: TerminalId) {
        let Some(page) = self.pages.remove(&chat) else {
            return;
        };
        if let Some(cdp) = &mut self.cdp {
            cdp.tell(
                None,
                "Target.disposeBrowserContext",
                json!({ "browserContextId": page.context }),
            );
        }
        self.publish_closed(chat, None);
        if self.pages.is_empty() {
            self.idle_since = Some(Instant::now());
        }
    }

    /// The browser started as `launch` closed its pipe. If it's the one
    /// running, every page goes with it.
    fn gone(&mut self, launch: u64) {
        if launch == self.launches && self.cdp.is_some() {
            self.stopped();
        }
    }

    /// The browser stopped by itself: every page with it.
    fn stopped(&mut self) {
        self.running.set(None);
        let Some(mut cdp) = self.cdp.take() else {
            return;
        };
        cdp.stop();
        log::warn!("the browser stopped");
        let chats: Vec<TerminalId> = self.pages.keys().copied().collect();
        self.pages.clear();
        for chat in chats {
            self.publish_closed(chat, Some("the browser stopped".to_owned()));
        }
    }

    /// Tells the daemon the chat has no page now, for `why` if it went
    /// wrong.
    fn publish_closed(&mut self, chat: TerminalId, why: Option<String>) {
        let status = BrowserStatus {
            failure: why,
            ..BrowserStatus::default()
        };
        self.lock().statuses.insert(chat, status);
        (self.wake)();
    }

    /// Asks the browser for the chat's page's address and title, and tells
    /// the daemon if they changed, or if `action` is something new it did.
    fn refresh(&mut self, chat: TerminalId, action: Option<String>) {
        let Some(page) = self.pages.get(&chat) else {
            return;
        };
        let target = page.target.clone();
        let info = self.browser_call("Target.getTargetInfo", json!({ "targetId": target }));
        let Some(page) = self.pages.get_mut(&chat) else {
            return;
        };
        let mut status = page.status.clone();
        if let Ok(info) = info {
            let info = &info["targetInfo"];
            info["url"]
                .as_str()
                .unwrap_or_default()
                .clone_into(&mut status.url);
            info["title"]
                .as_str()
                .unwrap_or_default()
                .clone_into(&mut status.title);
        }
        if action.is_some() {
            status.action = action;
        }
        if status != page.status {
            page.status = status.clone();
            locked(&self.outbox).statuses.insert(chat, status);
            (self.wake)();
        }
    }

    // --- Watching ---------------------------------------------------------

    fn watch(&mut self, chat: TerminalId, size: Option<[u32; 2]>) {
        match size {
            Some(size) => self.watching.insert(chat, size),
            None => self.watching.remove(&chat),
        };
        if self.pages.contains_key(&chat) {
            self.cast(chat, size);
        }
    }

    /// Starts sending pictures of the chat's page at `size`, or with
    /// `None`, stops.
    fn cast(&mut self, chat: TerminalId, size: Option<[u32; 2]>) {
        let Some(page) = self.pages.get_mut(&chat) else {
            return;
        };
        if page.casting == size {
            return;
        }
        let session = page.session.clone();
        let was = page.casting.take();
        page.ack = None;
        let Some(cdp) = &mut self.cdp else {
            return;
        };
        if was.is_some() {
            cdp.tell(Some(&session), "Page.stopScreencast", json!({}));
        }
        if let Some([width, height]) = size {
            let params = json!({
                "format": "jpeg",
                "quality": 60,
                "maxWidth": width.max(1),
                "maxHeight": height.max(1),
                "everyNthFrame": 1,
            });
            match cdp.call(
                Some(&session),
                "Page.startScreencast",
                params,
                cdp::ANSWER_WAIT,
            ) {
                Ok(_) => {
                    if let Some(page) = self.pages.get_mut(&chat) {
                        page.casting = size;
                    }
                }
                Err(why) => log::warn!("chat {chat}: its page can't be watched: {why}"),
            }
        }
    }

    // --- Events -----------------------------------------------------------

    fn event(&mut self, method: &str, params: &Value, session: Option<&str>) {
        let Some(session) = session else {
            return;
        };
        let Some((&chat, page)) = (self.pages.iter_mut()).find(|(_, page)| page.session == session)
        else {
            return;
        };
        match method {
            "Page.screencastFrame" => {
                let jpeg = params["data"]
                    .as_str()
                    .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok());
                let now = Instant::now();
                let due = page.acked.map_or(now, |acked| (acked + FRAME_GAP).max(now));
                page.ack = Some((due, params["sessionId"].clone()));
                if let Some(jpeg) = jpeg {
                    locked(&self.outbox).frames.insert(chat, jpeg);
                    (self.wake)();
                }
            }
            "Page.frameNavigated" if params["frame"].get("parentId").is_none() => {
                let url = params["frame"]["url"].as_str().unwrap_or_default();
                if page.status.url != url {
                    url.clone_into(&mut page.status.url);
                    let status = page.status.clone();
                    locked(&self.outbox).statuses.insert(chat, status);
                    (self.wake)();
                }
            }
            "Page.loadEventFired" => self.refresh(chat, None),
            "Page.javascriptDialogOpening" => {
                let kind = params["type"].as_str().unwrap_or("a");
                let message: String = (params["message"].as_str().unwrap_or_default().chars())
                    .take(200)
                    .collect();
                page.notes.push(format!(
                    "The page opened {kind} dialog, which was turned down: \"{message}\""
                ));
            }
            _ => {}
        }
    }

    /// Waits until `until`, doing what else comes meanwhile, for an event
    /// on `session` that `wanted` says it wants. Jobs that come are done
    /// after. Returns whether it came.
    fn wait_for(
        &mut self,
        session: &str,
        until: Instant,
        mut wanted: impl FnMut(&str, &Value) -> bool,
    ) -> bool {
        loop {
            let now = Instant::now();
            if now >= until {
                return false;
            }
            match self.heard.recv_timeout(until - now) {
                // The daemon is stopping: nothing more is waited for.
                Ok(Input::Job(Job::Stop)) => {
                    self.deferred.push_back(Job::Stop);
                    return false;
                }
                Ok(Input::Job(job)) => self.deferred.push_back(job),
                Ok(Input::Event {
                    method,
                    params,
                    session: from,
                }) => {
                    self.event(&method, &params, from.as_deref());
                    if from.as_deref() == Some(session) && wanted(&method, &params) {
                        return true;
                    }
                }
                Ok(Input::Gone(launch)) => {
                    self.gone(launch);
                    if self.cdp.is_none() {
                        return false;
                    }
                }
                Err(_) => return false,
            }
            self.chores();
        }
    }

    // --- Time -------------------------------------------------------------

    /// When something's next due: a picture to acknowledge, or the
    /// browser to stop for having had no pages a while.
    fn next_due(&self) -> Option<Instant> {
        let acks = self
            .pages
            .values()
            .filter_map(|page| Some(page.ack.as_ref()?.0));
        let idle = (self.cdp.is_some())
            .then_some(self.idle_since)
            .flatten()
            .map(|since| since + self.config.idle);
        acks.chain(idle).min()
    }

    /// Does what's due.
    fn chores(&mut self) {
        let now = Instant::now();
        let Some(cdp) = &mut self.cdp else {
            return;
        };
        for page in self.pages.values_mut() {
            if let Some((due, _)) = &page.ack
                && *due <= now
                && let Some((_, number)) = page.ack.take()
            {
                cdp.tell(
                    Some(&page.session),
                    "Page.screencastFrameAck",
                    json!({ "sessionId": number }),
                );
                page.acked = Some(now);
            }
        }
        if self.pages.is_empty()
            && let Some(since) = self.idle_since
            && since + self.config.idle <= now
        {
            log::info!("stopping the browser, as no chat has a page");
            self.idle_since = None;
            self.running.set(None);
            if let Some(mut cdp) = self.cdp.take() {
                cdp.stop();
            }
        }
    }

    /// Sends `method` to the page attached as `session`, and waits for the
    /// answer.
    fn call(&mut self, session: &str, method: &str, params: Value) -> Result<Value, String> {
        let cdp = self.cdp.as_mut().ok_or("the browser isn't running")?;
        cdp.call(Some(session), method, params, cdp::ANSWER_WAIT)
    }

    /// Sends `method` to the browser itself, and waits for the answer.
    fn browser_call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let cdp = self.cdp.as_mut().ok_or("the browser isn't running")?;
        cdp.call(None, method, params, cdp::ANSWER_WAIT)
    }
}

/// The outbox, locked.
fn locked(outbox: &Mutex<Outbox>) -> MutexGuard<'_, Outbox> {
    outbox.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The centre of a quad, `[x1, y1, … x4, y4]`, as Chromium gives one.
fn centre(quad: &Value) -> Option<[f64; 2]> {
    let points: Vec<f64> = quad.as_array()?.iter().filter_map(Value::as_f64).collect();
    let [x1, y1, x2, y2, x3, y3, x4, y4] = points.as_slice() else {
        return None;
    };
    Some([(x1 + x2 + x3 + x4) / 4.0, (y1 + y2 + y3 + y4) / 4.0])
}

/// `url` as the browser is to open it: with `https://` if it names no
/// scheme. Only web pages, files, `data:` and `about:blank` are opened;
/// `javascript:`, `chrome:` and the like are refused.
pub(crate) fn checked_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("give the address of a page to open".to_owned());
    }
    let scheme = url
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .filter(|scheme| {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "+-.".contains(ch))
        });
    match scheme.as_deref() {
        // `localhost:3000` is a host and a port, not a scheme.
        Some(scheme) if url[scheme.len() + 1..].starts_with(|ch: char| ch.is_ascii_digit()) => {
            Ok(format!("http://{url}"))
        }
        Some("http" | "https" | "file" | "data") => Ok(url.to_owned()),
        Some("about") if url.eq_ignore_ascii_case("about:blank") => Ok(url.to_owned()),
        Some(scheme) => Err(format!(
            "{scheme}: addresses aren't opened: only http, https, file and data ones are"
        )),
        None if url.starts_with("localhost") || url.starts_with("127.") => {
            Ok(format!("http://{url}"))
        }
        None => Ok(format!("https://{url}")),
    }
}

/// The key called `name`, as Chromium's key events give it: its `key`,
/// its Windows key code, and the text it types, if it types any. One
/// character is itself.
fn key_named(name: &str) -> Option<(String, u32, Option<String>)> {
    let mut chars = name.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        let code = if ch.is_ascii_alphanumeric() {
            u32::from(ch.to_ascii_uppercase())
        } else {
            0
        };
        return Some((ch.to_string(), code, Some(ch.to_string())));
    }
    let (key, code, text): (&str, u32, Option<&str>) = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => ("Enter", 13, Some("\r")),
        "tab" => ("Tab", 9, None),
        "escape" | "esc" => ("Escape", 27, None),
        "backspace" => ("Backspace", 8, None),
        "delete" => ("Delete", 46, None),
        "space" => (" ", 32, Some(" ")),
        "arrowup" | "up" => ("ArrowUp", 38, None),
        "arrowdown" | "down" => ("ArrowDown", 40, None),
        "arrowleft" | "left" => ("ArrowLeft", 37, None),
        "arrowright" | "right" => ("ArrowRight", 39, None),
        "home" => ("Home", 36, None),
        "end" => ("End", 35, None),
        "pageup" => ("PageUp", 33, None),
        "pagedown" => ("PageDown", 34, None),
        _ => return None,
    };
    Some((key.to_owned(), code, text.map(str::to_owned)))
}

/// The browser to run, and its profile, as `config` says, or why there's
/// none.
fn locate(config: &BrowserConfig) -> Result<(PathBuf, PathBuf), String> {
    let program = if let Some(program) = &config.program {
        program.clone()
    } else {
        let found: Vec<PathBuf> = (PROGRAMS.iter())
            .filter_map(|name| clackatoo_syntax::servers::on_path(name))
            .collect();
        (found.iter().find(|program| snap_name(program).is_none()))
            .or(found.first())
            .cloned()
            .ok_or(
                "no Chromium or Chrome was found on PATH: install one, or set \
                 CLACKATOO_BROWSER to one's path",
            )?
    };
    let profile = match (&config.profile, snap_name(&program)) {
        (Some(profile), _) => profile.clone(),
        (None, Some(snap)) => {
            (config.home.join("snap").join(snap)).join("common/clackatoo-browser")
        }
        (None, None) => (config.data.clone()).ok_or(
            "there's nowhere to keep the browser's profile: set CLACKATOO_BROWSER_PROFILE",
        )?,
    };
    Ok((program, profile))
}

/// The snap `program` runs, if it's one: snaps' programs are links to
/// `snap` itself, named for the snap.
fn snap_name(program: &Path) -> Option<String> {
    let real = program.canonicalize().ok()?;
    let is_snap = real.file_name().is_some_and(|name| name == "snap")
        || program.starts_with("/snap/")
        || real.starts_with("/snap/");
    let name = program.file_name()?.to_str()?;
    is_snap.then(|| name.split('.').next().unwrap_or(name).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pages_files_and_data_are_opened() {
        assert_eq!(checked_url("example.com").unwrap(), "https://example.com");
        assert_eq!(
            checked_url(" https://example.com/a?b ").unwrap(),
            "https://example.com/a?b"
        );
        assert_eq!(
            checked_url("localhost:3000/x").unwrap(),
            "http://localhost:3000/x"
        );
        assert_eq!(checked_url("127.0.0.1").unwrap(), "http://127.0.0.1");
        assert_eq!(
            checked_url("file:///etc/hosts").unwrap(),
            "file:///etc/hosts"
        );
        assert!(checked_url("data:text/html,<p>hi</p>").is_ok());
        assert!(checked_url("about:blank").is_ok());
        assert!(
            checked_url("javascript:alert(1)")
                .unwrap_err()
                .contains("javascript:")
        );
        assert!(checked_url("chrome://settings").is_err());
        assert!(checked_url("about:config").is_err());
        assert!(checked_url("  ").is_err());
    }

    #[test]
    fn keys_are_named_or_typed() {
        assert_eq!(
            key_named("Enter"),
            Some(("Enter".to_owned(), 13, Some("\r".to_owned())))
        );
        assert_eq!(key_named("arrowdown").unwrap().1, 40);
        assert_eq!(
            key_named("a"),
            Some(("a".to_owned(), u32::from(b'A'), Some("a".to_owned())))
        );
        assert_eq!(key_named("Hyper"), None);
    }

    #[test]
    fn a_quads_centre_is_between_its_corners() {
        let quad = json!([10.0, 20.0, 30.0, 20.0, 30.0, 40.0, 10.0, 40.0]);
        assert_eq!(centre(&quad), Some([20.0, 30.0]));
        assert_eq!(centre(&json!([1.0])), None);
    }

    #[test]
    fn a_profile_goes_where_the_browser_can_reach_it() {
        let config = BrowserConfig {
            program: Some(PathBuf::from("/usr/bin/true")),
            profile: None,
            data: Some(PathBuf::from("/data/clackatoo/browser")),
            home: PathBuf::from("/home/me"),
            idle: Duration::ZERO,
        };
        let (_, profile) = locate(&config).unwrap();
        assert_eq!(profile, Path::new("/data/clackatoo/browser"));
        let chosen = BrowserConfig {
            profile: Some(PathBuf::from("/mine")),
            ..config
        };
        assert_eq!(locate(&chosen).unwrap().1, Path::new("/mine"));
    }
}
