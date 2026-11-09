//! One process for every window: its event loop, the windows open in it,
//! and what they share.
//!
//! Setting up the GPU is most of the time a window takes to open, so the
//! process of windows does it once, for its first window, and every
//! window after opens on the same device, in a few milliseconds. It's
//! forked off the `clackatoo` that asks for its first window, apart from
//! any terminal (see [`crate::handoff`]), and `clackatoo` asks it for each
//! window after; a window opening one on another machine opens it here
//! too.
//!
//! What's shared is only what's slow to make and the same for every
//! window: the GPU, and the fonts found on the system. Everything else is
//! each window's own, as it was with a process each: its desk, its frame
//! and glyphs (at its screen's scale), its connection to its daemon, on
//! this machine or another, its config's Lua, and the environment and
//! directory of the `clackatoo` that asked for it, which what it starts
//! (the daemon, ssh, a link's opener, `clackatoo update`) is started with.
//!
//! A window that panics closes alone: everything done for a window runs
//! inside [`catch_unwind`], and nothing it holds is touched again. A crash
//! outside our code, in the GPU driver say, still takes every window: the
//! price of sharing the device. The daemon holds everything they showed.
//! If the device is lost, each window opens again in a process started
//! afresh, one window first and the others once its process listens, and
//! this one ends; if devices keep being lost, the windows close instead.
//!
//! Every window shares the one event loop, so a window stalls the others
//! while it handles something: opening a window waits for the compositor's
//! first answer (a few milliseconds), and a frame waits for the surface;
//! windows the compositor says are hidden aren't drawn, so one never waits
//! on a surface that won't take frames.
//!
//! The process ends with its last window, as a window's did: a device
//! kept for no window would keep the driver waking.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::launch::Origin;
use clackatoo_daemon::remote::Remote;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, AsyncRequestSerial, ControlFlow, EventLoopProxy};
use winit::window::WindowId;

use super::{App, Gpu, Pending, Startup};
use crate::desktop::Desktop;
use crate::fonts::Fonts;
use crate::handoff::{self, Asked, Build, Listening};
use crate::launch::LaunchToken;
use crate::session::{Closed, Location, Proxy, Session, SessionEvent};
use crate::{FontFixed, FontOptions, GraphicsApi, cmdline, desk, lua, options, tutor, updates};

/// How long a window opened from another waits for the token the desktop
/// gives the other to hand it, before opening without.
const TOKEN_WAIT: Duration = Duration::from_millis(500);

/// How long a process of windows waits for its first window to be asked
/// for, before ending with none, as when its asker went.
const FIRST_WAIT: Duration = Duration::from_secs(10);

/// How many times within [`LOSSES_WITHIN`] the GPU may be lost before the
/// windows close rather than open again (the third loss closes them): a
/// GPU that can't be kept is lost again by every process opened on it.
const MOST_LOSSES: usize = 3;
const LOSSES_WITHIN: Duration = Duration::from_mins(1);

/// Losses noted this close together, by processes of windows each losing
/// the same device, are one.
const ONE_LOSS: Duration = Duration::from_secs(2);

/// How long, after the GPU is lost, the windows wait for the process the
/// first window opens again in to listen, to open again in it too.
const NEW_PROCESS_WAIT: Duration = Duration::from_secs(5);

/// One of the process's windows, as events from its threads address it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WindowKey(u64);

/// What wakes the event loop, from another thread.
#[derive(Debug)]
pub(crate) enum Wake {
    /// An event for one window.
    Window(WindowKey, SessionEvent),
    /// Another process asks for a window.
    Asked(Asked),
    /// The GPU's device was lost.
    GpuLost,
    /// The desktop's preferences, read again for the next window.
    Desktop(Desktop),
}

/// What wakes the event loop from other threads: its proxy, or in tests,
/// a channel.
#[derive(Clone, Debug)]
pub(crate) enum Waker {
    EventLoop(EventLoopProxy<Wake>),
    #[cfg(test)]
    Channel(std::sync::mpsc::Sender<Wake>),
}

impl Waker {
    /// Wakes the event loop with `wake`. It fails only once the process is
    /// ending.
    pub(crate) fn wake(&self, wake: Wake) -> Result<(), Closed> {
        match self {
            Self::EventLoop(proxy) => proxy.send_event(wake).map_err(|_| Closed),
            #[cfg(test)]
            Self::Channel(sender) => sender.send(wake).map_err(|_| Closed),
        }
    }
}

/// What a window opens with, as `clackatoo`'s arguments say, and where it
/// was asked for from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Spec {
    /// The other machine whose daemon to use, if any.
    pub(crate) remote: Option<Remote>,
    /// Whether to open as the picker of a machine to connect to.
    pub(crate) pick_remote: bool,
    /// The shell command that runs clackatoo on another machine.
    pub(crate) remote_command: String,
    pub(crate) font: FontOptions,
    pub(crate) font_fixed: FontFixed,
    /// The environment and directory of the `clackatoo` that asked for the
    /// window, which what the window starts is started with: this
    /// process's own, by default.
    pub(crate) origin: Origin,
}

/// What's the same for every window in the process.
#[derive(Debug)]
pub(crate) struct Process {
    /// Where the daemon is, or will be started, and windows are asked for.
    pub(crate) runtime: RuntimeDir,
    /// clackatoo's config directory, whose `client/init.lua` configures
    /// each window.
    pub(crate) config: Option<PathBuf>,
    /// The graphics API the process draws with.
    pub(crate) graphics_api: GraphicsApi,
    /// Whether its thread asked to run ahead of the user's other programs.
    pub(crate) priority: bool,
    /// The socket other processes ask this one for windows on, if they do:
    /// `window.sock`, or its group's (see [`handoff::group_socket`]).
    pub(crate) listen: Option<PathBuf>,
    /// Whether the windows its windows open, as the picker's new window,
    /// open in this process: not if every window is to have a process of
    /// its own (see [`handoff::OFF_VAR`]).
    pub(crate) share: bool,
}

/// A window to open from another, waiting for the token the desktop gives
/// the other to hand it, so the new one takes focus.
#[derive(Debug)]
pub(crate) struct Opening {
    /// The request for the token.
    pub(crate) serial: AsyncRequestSerial,
    /// When to stop waiting and open without.
    pub(crate) until: Instant,
    pub(crate) spec: Spec,
}

impl Opening {
    pub(crate) fn new(serial: AsyncRequestSerial, spec: Spec) -> Self {
        Self {
            serial,
            until: Instant::now() + TOKEN_WAIT,
            spec,
        }
    }
}

/// What a window knows of the process it's in, and asks of it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Link(Rc<Linked>);

#[derive(Debug, Default)]
struct Linked {
    /// The build the process runs, if it opens the windows its windows ask
    /// for: a window drawn offscreen (see [`super::look`]), or in a process
    /// of its own, opens them in processes of their own.
    build: Option<Build>,
    /// Where the process's program was as it started: a build put in its
    /// place since is there, and the one running may be gone from there.
    program: Option<PathBuf>,
    /// The graphics API the process draws with, if it's known.
    renderer: Option<GraphicsApi>,
    /// Windows asked for, to open once the event that asked is handled.
    asked: RefCell<Vec<(Spec, LaunchToken)>>,
    /// Whether the GPU was lost: windows go elsewhere.
    lost: Cell<bool>,
}

impl Link {
    /// Whether the process opens windows for its windows.
    pub(crate) fn hosting(&self) -> bool {
        self.0.build.is_some() && !self.0.lost.get()
    }

    /// Asks the process to open a window for `spec`, with `launch`'s token.
    pub(crate) fn open(&self, spec: Spec, launch: LaunchToken) {
        self.0.asked.borrow_mut().push((spec, launch));
    }

    /// Where the program the process started from was, if it can be
    /// found: where windows find the build to update, or open, from.
    pub(crate) fn program(&self) -> Option<&Path> {
        self.0.program.as_deref()
    }

    /// The graphics API the process draws with, if it's known.
    pub(crate) fn renderer(&self) -> Option<GraphicsApi> {
        self.0.renderer
    }

    /// Whether `program`, which the process was started from, is another
    /// build now, as when `clackatoo update` or a deploy put one in its
    /// place: windows are opened by the new build from then on.
    pub(crate) fn replaced(&self, program: &Path) -> bool {
        self.0
            .build
            .as_ref()
            .is_some_and(|build| Build::of(program).as_ref() != Some(build))
    }
}

/// What windows share, made for the first to open.
struct Shared {
    gpu: Gpu,
    /// The system's fonts, as found: copies share what any finds.
    fonts: fontique::Collection,
    desktop: Desktop,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("gpu", &self.gpu)
            .field("desktop", &self.desktop)
            .finish_non_exhaustive()
    }
}

/// The fonts and GPU being made as the process starts, for the first
/// window asked for.
type Starting = (Pending<(Fonts, Desktop)>, Pending<Gpu>);

/// The process's windows, and the event loop's handler.
#[derive(Debug)]
pub(crate) struct Host {
    process: Process,
    waker: Waker,
    link: Link,
    windows: HashMap<WindowKey, App>,
    /// The windows by their desktop window.
    ids: HashMap<WindowId, WindowKey>,
    /// When each window asked to be woken, if it did.
    wakes: HashMap<WindowKey, Instant>,
    next: u64,
    starting: Option<Starting>,
    /// Whether its first window is still opening: windows asked for
    /// meanwhile are taken on at once, to open once it can.
    starting_up: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shared: Option<Shared>,
    listening: Option<Listening>,
    /// Whether the event loop can make windows yet.
    resumed: bool,
    /// Windows asked for before it could.
    waiting: Vec<Asked>,
    /// When the process started, and whether a window has opened since.
    born: Instant,
    opened_any: bool,
    /// Whether the process is to end, once the event that ended it is
    /// handled.
    ending: bool,
    /// Why the process ends badly, if it does: its last window failed.
    pub(crate) result: anyhow::Result<()>,
}

impl Host {
    pub(crate) fn new(process: Process, waker: Waker) -> Self {
        let link = Linked {
            build: process.share.then(Build::running).flatten(),
            program: std::env::current_exe()
                .inspect_err(|err| log::warn!("this program can't be found: {err}"))
                .ok(),
            renderer: Some(process.graphics_api),
            ..Linked::default()
        };
        Self {
            process,
            waker,
            link: Link(Rc::new(link)),
            windows: HashMap::new(),
            ids: HashMap::new(),
            wakes: HashMap::new(),
            next: 0,
            starting: None,
            starting_up: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            shared: None,
            listening: None,
            resumed: false,
            waiting: Vec::new(),
            born: Instant::now(),
            opened_any: false,
            ending: false,
            result: Ok(()),
        }
    }

    /// The fonts and GPU being made as the process starts, for the first
    /// window asked for, `first`.
    pub(crate) fn starting(
        &mut self,
        fonts: Pending<(Fonts, Desktop)>,
        gpu: Pending<Gpu>,
        first: Asked,
    ) {
        self.starting = Some((fonts, gpu));
        self.waiting.push(first);
    }

    /// Makes a window for `spec`, drawing with `start`'s options until
    /// its config says otherwise, to open with `startup` once the event
    /// loop can. Its start is timed from `started`.
    pub(crate) fn add(
        &mut self,
        spec: Spec,
        start: options::Options,
        started: Instant,
        startup: Startup,
    ) -> WindowKey {
        let key = WindowKey(self.next);
        self.next += 1;
        let proxy = || Proxy::EventLoop {
            waker: self.waker.clone(),
            window: key,
        };
        let runtime = &self.process.runtime;
        // Two small files read, and a directory or so looked at.
        let tutor = tutor::Start::read(spec.remote.is_some() || spec.pick_remote);
        let recall = cmdline::recall::Start::read();
        // The config runs alongside everything else starting.
        let lua_start = self.process.config.clone().map(|dir| {
            let proxy = proxy();
            desk::LuaStart {
                source: lua::Source::Dir(dir),
                wake: Box::new(move || -> lua::Wake {
                    let proxy = proxy.clone();
                    Box::new(move || {
                        let _ = proxy.send_event(SessionEvent::Lua);
                    })
                }),
            }
        });
        let mut session = Session::waiting(runtime, proxy()).from(spec.origin.clone());
        if !spec.pick_remote {
            session.connect(match &spec.remote {
                Some(remote) => Location::Remote(remote.clone()),
                None => Location::Local(runtime.clone()),
            });
        }
        updates::after_opening(proxy());
        let fixed = spec.font_fixed.options();
        let pick = spec.pick_remote;
        let mut app = App::new(started, Some(startup), session, spec, self.link.clone());
        // Before config runs, which starts from them, with what the process
        // started with, which only a new process changes.
        let start = options::Options {
            renderer: self.process.graphics_api.into(),
            priority: if self.process.priority {
                options::Priority::High
            } else {
                options::Priority::Normal
            },
            ..start
        };
        app.start_options(start, fixed);
        if pick {
            app.pick_machine();
        }
        if let Some(start) = lua_start {
            app.start_lua(start);
        }
        app.start_tutor(tutor);
        app.start_recall(recall);
        self.windows.insert(key, app);
        key
    }

    /// Opens the window `key` was made for. Returns why not, if it
    /// couldn't, having closed it.
    fn open(&mut self, event_loop: &ActiveEventLoop, key: WindowKey) -> anyhow::Result<()> {
        if let Some(closed) = self.on_window(key, |app| app.resume(event_loop)) {
            return closed.and(Err(anyhow!("the window closed as it opened")));
        }
        let app = &self.windows[&key];
        if let Some(id) = app.window_id() {
            self.ids.insert(id, key);
        }
        self.opened_any = true;
        self.starting_up
            .store(false, std::sync::atomic::Ordering::Release);
        if self.shared.is_none()
            && let Some(frame) = &app.frame
        {
            let waker = self.waker.clone();
            frame.gpu.lost.notify(Box::new(move || {
                let _ = waker.wake(Wake::GpuLost);
            }));
            self.shared = Some(Shared {
                gpu: frame.gpu.clone(),
                fonts: frame.fonts.font_cx.collection.clone(),
                desktop: frame.desktop.clone(),
            });
        }
        self.read_desktop();
        Ok(())
    }

    /// Reads the desktop's preferences again, on a thread of its own, for
    /// the next window: the interface font and where the window's buttons
    /// go may have changed since the first window opened. (The system's
    /// fonts aren't found again: a font installed since shows in windows
    /// once the process of windows has ended and started again.)
    fn read_desktop(&self) {
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new()
            .name("desktop".into())
            .spawn(move || {
                let _ = waker.wake(Wake::Desktop(Desktop::read()));
            });
        if let Err(err) = spawned {
            log::info!("the desktop's preferences aren't read again: {err}");
        }
    }

    /// What another window opens with: what the first made, or for the
    /// first, what's being made as the process starts.
    fn startup(&mut self, spec: &Spec, launch: LaunchToken) -> anyhow::Result<Startup> {
        if let Some(shared) = &self.shared {
            let start = options::Options::drawing_with(&spec.font);
            let fonts =
                Fonts::for_window_in(shared.fonts.clone(), &start.font_options(), &shared.desktop)?;
            return Ok(Startup {
                fonts: Pending::Ready((fonts, shared.desktop.clone())),
                gpu: Pending::Ready(shared.gpu.clone()),
                launch,
            });
        }
        let (fonts, gpu) = self.starting.take().context("no GPU is open here")?;
        Ok(Startup { fonts, gpu, launch })
    }

    /// Opens another window, for `spec`, timing its start from `started`.
    /// A panic as it's made is the window's: it isn't opened.
    fn open_another(
        &mut self,
        event_loop: &ActiveEventLoop,
        spec: Spec,
        launch: LaunchToken,
        started: Instant,
    ) -> anyhow::Result<WindowKey> {
        let made = catch_unwind(AssertUnwindSafe(|| -> anyhow::Result<WindowKey> {
            let startup = self.startup(&spec, launch)?;
            let start = options::Options::drawing_with(&spec.font);
            Ok(self.add(spec, start, started, startup))
        }));
        let key = match made {
            Ok(made) => made?,
            Err(panic) => anyhow::bail!("making the window failed: {}", said(&*panic)),
        };
        self.open(event_loop, key)?;
        Ok(key)
    }

    /// Opens the window another process asked for, if it still waits.
    fn asked(&mut self, event_loop: &ActiveEventLoop, mut asked: Asked) {
        if !self.resumed {
            self.waiting.push(asked);
            return;
        }
        // Dropped unanswered: the asker opens it elsewhere.
        if self.ending || self.link.0.lost.get() {
            log::info!("a window was asked for as the process of windows ends");
            return;
        }
        if !asked.take() {
            log::info!("a window was asked for, but the asker has stopped waiting");
            return;
        }
        let opened = self.open_another(
            event_loop,
            asked.spec.clone(),
            asked.launch.clone(),
            asked.at,
        );
        let opened = match opened {
            // The asker gave up waiting, and has opened its own.
            Ok(key) if asked.late() => {
                log::warn!("a window asked for opened too late, and closes again");
                let _ = self.close(key, None);
                Err("it opened too late".to_owned())
            }
            Ok(_) => Ok(()),
            Err(err) => {
                log::warn!("couldn't open a window asked for: {err:#}");
                Err(format!("{err:#}"))
            }
        };
        asked.opened(opened);
    }

    /// Opens the windows the process's windows asked for.
    fn open_asked(&mut self, event_loop: &ActiveEventLoop) {
        let asked = std::mem::take(&mut *self.link.0.asked.borrow_mut());
        for (spec, launch) in asked {
            if let Err(err) = self.open_another(event_loop, spec, launch, Instant::now()) {
                log::warn!("couldn't open a window: {err:#}");
            }
        }
    }

    /// Starts taking windows asked for by other processes, if it does: as
    /// the process starts, before its event loop runs, so they wait for it
    /// rather than start processes of their own.
    pub(crate) fn listen(&mut self) {
        let Some(at) = self.process.listen.clone() else {
            return;
        };
        if self.listening.is_some() || self.link.0.lost.get() {
            return;
        }
        let waker = self.waker.clone();
        let deliver = move |asked| {
            let _ = waker.wake(Wake::Asked(asked));
        };
        let graphics_api = self.process.graphics_api;
        let starting = std::sync::Arc::clone(&self.starting_up);
        match handoff::listen(&self.process.runtime, at, graphics_api, starting, deliver) {
            Ok(listening) => self.listening = Some(listening),
            Err(err) => log::warn!("windows asked for get processes of their own: {err:#}"),
        }
    }

    /// The GPU's device is gone. Nothing more can be drawn with it, so
    /// windows asked for go elsewhere from now on, and each window opens
    /// again in a process started afresh, and closes here.
    ///
    /// The process that notes the loss first leads: its first window opens
    /// again at once. Its other windows, and every window of other
    /// processes of windows losing the same device, wait for a process of
    /// windows to listen, and open again in it, rather than race to start
    /// one each. The loop is held meanwhile: nothing can be drawn anyway.
    fn gpu_lost(&mut self) {
        if self.link.0.lost.replace(true) {
            return;
        }
        self.listening = None;
        self.shared = None;
        self.starting = None;
        let mut keys: Vec<WindowKey> = self.windows.keys().copied().collect();
        keys.sort_by_key(|key| key.0);
        let noted = note_gpu_loss(&self.process.runtime);
        if noted.losses >= MOST_LOSSES {
            log::error!(
                "the GPU was lost {} times within {LOSSES_WITHIN:?}: the windows close",
                noted.losses
            );
            for key in keys {
                self.on_window(key, |app| app.closing = true);
            }
            return;
        }
        log::error!("the GPU was lost: the windows open again in a new process");
        let mut keys = keys.into_iter();
        if noted.leader {
            // A moment more each time it's lost again.
            let exponent = u32::try_from(noted.losses.saturating_sub(1))
                .unwrap_or(u32::MAX)
                .min(4);
            std::thread::sleep(Duration::from_millis(250) * 2_u32.pow(exponent));
            if let Some(first) = keys.next() {
                self.on_window(first, App::open_elsewhere);
            }
        }
        if keys.len() > 0 && !another_listens(&self.process.runtime, NEW_PROCESS_WAIT) {
            log::warn!("no new process of windows listens: each window starts its own");
        }
        for key in keys {
            self.on_window(key, App::open_elsewhere);
        }
    }

    /// Runs `handle` on the window `key`, if it's open. If that closes it,
    /// or panics, the window closes, and this returns how it went.
    fn on_window(
        &mut self,
        key: WindowKey,
        handle: impl FnOnce(&mut App),
    ) -> Option<anyhow::Result<()>> {
        let app = self.windows.get_mut(&key)?;
        match guarded(app, handle) {
            Handled::Open => None,
            Handled::Closing => Some(self.close(key, None)),
            Handled::Panicked(why) => Some(self.close(key, Some(why))),
        }
    }

    /// Closes the window `key`, which panicked if `panicked` says why, and
    /// returns how it went. The process ends with its last window.
    fn close(&mut self, key: WindowKey, panicked: Option<String>) -> anyhow::Result<()> {
        let Some(mut app) = self.windows.remove(&key) else {
            return Ok(());
        };
        self.ids.retain(|_, open| *open != key);
        self.wakes.remove(&key);
        let last = self.windows.is_empty();
        // What a window that panicked holds may be half changed: nothing
        // more is asked of it.
        let result = if let Some(why) = panicked {
            log::error!("a window failed, and closes: {why}");
            Err(anyhow!("the window failed: {why}"))
        } else {
            let closed = catch_unwind(AssertUnwindSafe(|| app.closed(last)));
            let result = std::mem::replace(&mut app.result, Ok(()));
            result.and(closed.map_err(|panic| anyhow!("closing failed: {}", said(&*panic))))
        };
        if catch_unwind(AssertUnwindSafe(move || drop(app))).is_err() {
            log::error!("a window failed as it closed");
        }
        if last {
            if let Err(err) = &result {
                self.result = Err(anyhow!("{err:#}"));
            }
            // Windows asked for from now on go to a process that stays.
            self.listening = None;
            self.ending = true;
        } else if let Err(err) = &result {
            log::error!("{err:#}");
        }
        result
    }

    /// What's due once an event is handled: the windows the windows asked
    /// for opened, and the process ended, if its last window has closed.
    fn settle(&mut self, event_loop: &ActiveEventLoop) {
        self.open_asked(event_loop);
        if self.ending && self.windows.is_empty() {
            event_loop.exit();
        }
    }

    /// Handles `wake`, from another thread, but for a window asked for,
    /// which needs the event loop.
    fn woken(&mut self, wake: Wake) -> Option<Asked> {
        match wake {
            // A window that closed as it connected: its session thread
            // waits on the connection until it's hung up.
            Wake::Window(key, SessionEvent::Connected { sender, .. })
                if !self.windows.contains_key(&key) =>
            {
                sender.close();
            }
            Wake::Window(key, event) => {
                self.on_window(key, |app| {
                    // Every workspace has closed.
                    if app.session_event(event) {
                        app.closing = true;
                    }
                });
            }
            Wake::Asked(asked) => return Some(asked),
            Wake::GpuLost => self.gpu_lost(),
            Wake::Desktop(desktop) => {
                if let Some(shared) = &mut self.shared {
                    shared.desktop = desktop;
                }
            }
        }
        None
    }
}

/// A GPU loss, as noted.
#[derive(Debug, PartialEq, Eq)]
struct Noted {
    /// Whether this process noted it first, and leads.
    leader: bool,
    /// How many times the GPU has been lost within [`LOSSES_WITHIN`], this
    /// time included.
    losses: usize,
}

/// Notes the GPU lost now, in `gpu-lost` in `runtime`, where every process
/// of windows notes it, holding the lock on `gpu-lost.lock` meanwhile,
/// unless another process of windows has just noted it, losing the same
/// device. The file is a time in milliseconds a line, replaced whole.
fn note_gpu_loss(runtime: &RuntimeDir) -> Noted {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    note_loss(&runtime.path().join("gpu-lost"), now)
}

/// [`note_gpu_loss`], in the file at `path`, at `now` since the epoch.
fn note_loss(path: &Path, now: Duration) -> Noted {
    let now = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))
        .and_then(|lock| lock.lock().map(|()| lock));
    if let Err(err) = &lock {
        log::warn!("the GPU's losses are counted without a lock: {err}");
    }
    let within = u64::try_from(LOSSES_WITHIN.as_millis()).unwrap_or(u64::MAX);
    let mut losses: Vec<u64> = (std::fs::read_to_string(path).unwrap_or_default().lines())
        .filter_map(|line| line.parse::<u64>().ok())
        .filter(|&lost| now.saturating_sub(lost) < within)
        .collect();
    let one = u64::try_from(ONE_LOSS.as_millis()).unwrap_or(u64::MAX);
    if losses
        .last()
        .is_some_and(|&last| now.saturating_sub(last) < one)
    {
        return Noted {
            leader: false,
            losses: losses.len(),
        };
    }
    losses.push(now);
    let text = (losses.iter().map(|lost| format!("{lost}\n")))
        .collect::<Vec<_>>()
        .concat();
    let written = path.with_extension(format!("{}", std::process::id()));
    let replaced = std::fs::write(&written, text).and_then(|()| std::fs::rename(&written, path));
    if let Err(err) = replaced {
        let _ = std::fs::remove_file(&written);
        log::warn!("couldn't note the GPU lost: {err}");
    }
    drop(lock);
    Noted {
        leader: true,
        losses: losses.len(),
    }
}

/// Waits up to `wait` for a process of windows other than this one to
/// listen in `runtime`, and says whether one does.
fn another_listens(runtime: &RuntimeDir, wait: Duration) -> bool {
    let socket = handoff::shared_socket(runtime);
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(&socket)
            && rustix::net::sockopt::socket_peercred(&stream)
                .is_ok_and(|peer| peer.pid != rustix::process::getpid())
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// How a window came out of handling something.
#[derive(Debug, PartialEq, Eq)]
enum Handled {
    Open,
    /// It asked to close.
    Closing,
    /// It panicked, saying this.
    Panicked(String),
}

/// Runs `handle` on `app`, catching a panic.
fn guarded(app: &mut App, handle: impl FnOnce(&mut App)) -> Handled {
    match catch_unwind(AssertUnwindSafe(|| handle(app))) {
        Ok(()) if !app.closing => Handled::Open,
        Ok(()) => Handled::Closing,
        Err(panic) => Handled::Panicked(said(&*panic)),
    }
}

/// What a panic said.
fn said(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|said| (*said).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "it panicked".into())
}

impl ApplicationHandler<Wake> for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if std::mem::replace(&mut self.resumed, true) {
            return;
        }
        let made: Vec<WindowKey> = self.windows.keys().copied().collect();
        for key in made {
            // Its failure is the process's, if it's the last window.
            let _ = self.open(event_loop, key);
        }
        for asked in std::mem::take(&mut self.waiting) {
            self.asked(event_loop, asked);
        }
        self.listen();
        self.settle(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(&key) = self.ids.get(&id) {
            self.on_window(key, |app| {
                app.handle_window_event(event);
                app.send_outbox();
            });
        }
        self.settle(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, wake: Wake) {
        if let Some(asked) = self.woken(wake) {
            self.asked(event_loop, asked);
        }
        self.settle(event_loop);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            let now = Instant::now();
            let due: Vec<WindowKey> = (self.wakes.iter())
                .filter(|(_, wake)| **wake <= now)
                .map(|(key, _)| *key)
                .collect();
            for key in due {
                self.on_window(key, |app| app.woke(now));
            }
            self.settle(event_loop);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let keys: Vec<WindowKey> = self.windows.keys().copied().collect();
        for key in keys {
            let mut wake = None;
            self.on_window(key, |app| wake = app.idle());
            match wake {
                Some(wake) => self.wakes.insert(key, wake),
                None => self.wakes.remove(&key),
            };
        }
        // Started for a window, and none asked for: another process took
        // the socket first, or the asker went.
        let unasked = !self.opened_any && self.windows.is_empty() && self.waiting.is_empty();
        if unasked && self.born.elapsed() >= FIRST_WAIT {
            log::info!("no window was asked for: the process of windows ends");
            self.ending = true;
        }
        self.settle(event_loop);
        let first = unasked.then_some(self.born + FIRST_WAIT);
        let flow = match self.wakes.values().copied().chain(first).min() {
            Some(wake) => ControlFlow::WaitUntil(wake),
            None => ControlFlow::Wait,
        };
        event_loop.set_control_flow(flow);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Others stop asking this process for windows first.
        self.listening = None;
        for app in self.windows.values_mut() {
            if catch_unwind(AssertUnwindSafe(|| app.closed(true))).is_err() {
                log::error!("a window failed as it closed");
            }
        }
    }
}

#[cfg(test)]
mod tests;
