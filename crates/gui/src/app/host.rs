//! One process for every window: its event loop, the windows open in it,
//! and what they share.
//!
//! Setting up the GPU is most of the time a window takes to open, so a
//! process does it once, for its first window, and every window after
//! opens on the same device, in a few milliseconds. `clackatoo` asks the
//! process with windows open for another (see [`crate::handoff`]), and a
//! window opening one on another machine opens it here too.
//!
//! What's shared is only what's slow to make and the same for every
//! window: the GPU, and the fonts found on the system. Everything else is
//! each window's own, as it was with a process each: its desk, its frame
//! and glyphs (at its screen's scale), its connection to its daemon, on
//! this machine or another, and its config's Lua.
//!
//! A window that panics closes alone: everything it does runs inside
//! [`catch_unwind`], and nothing it holds is touched again. A crash
//! outside our code, in the GPU driver say, still takes every window: the
//! price of sharing the device. The daemon holds everything they showed.
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
use crate::session::{Location, Proxy, Session, SessionEvent};
use crate::{FontFixed, FontOptions, GraphicsApi, cmdline, desk, lua, options, tutor, updates};

/// How long a window opened from another waits for the token the desktop
/// gives the other to hand it, before opening without.
const TOKEN_WAIT: Duration = Duration::from_millis(500);

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
}

/// What a window opens with, as `clackatoo`'s arguments say.
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
    /// Whether other processes ask this one for windows: not if one with
    /// windows open already keeps them (see [`handoff`]).
    pub(crate) hosting: bool,
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
    /// The build the process runs, if it hosts windows: a window drawn
    /// offscreen (see [`super::look`]) is alone in its process.
    build: Option<Build>,
    /// Where the process's program was as it started: a build put in its
    /// place since is there, and the one running may be gone from there.
    program: Option<PathBuf>,
    /// How many windows are open.
    windows: Cell<usize>,
    /// Windows asked for, to open once the event that asked is handled.
    asked: RefCell<Vec<(Spec, LaunchToken)>>,
}

impl Link {
    /// Whether the process opens windows for its windows.
    pub(crate) fn hosting(&self) -> bool {
        self.0.build.is_some()
    }

    /// Whether the window is the only one in its process.
    pub(crate) fn alone(&self) -> bool {
        self.0.windows.get() <= 1
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

/// The process's windows, and the event loop's handler.
#[derive(Debug)]
pub(crate) struct Host {
    process: Process,
    proxy: EventLoopProxy<Wake>,
    link: Link,
    windows: HashMap<WindowKey, App>,
    /// The windows by their desktop window.
    ids: HashMap<WindowId, WindowKey>,
    /// When each window asked to be woken, if it did.
    wakes: HashMap<WindowKey, Instant>,
    next: u64,
    shared: Option<Shared>,
    listening: Option<Listening>,
    /// Why the process ends badly, if it does: its last window failed.
    pub(crate) result: anyhow::Result<()>,
}

impl Host {
    pub(crate) fn new(process: Process, proxy: EventLoopProxy<Wake>) -> Self {
        Self {
            process,
            proxy,
            link: Link(Rc::new(Linked {
                build: Build::running(),
                program: std::env::current_exe()
                    .inspect_err(|err| log::warn!("this program can't be found: {err}"))
                    .ok(),
                ..Linked::default()
            })),
            windows: HashMap::new(),
            ids: HashMap::new(),
            wakes: HashMap::new(),
            next: 0,
            shared: None,
            listening: None,
            result: Ok(()),
        }
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
            proxy: self.proxy.clone(),
            window: key,
        };
        let runtime = &self.process.runtime;
        // Two small files read, and a directory or so looked at.
        let tutor = tutor::Start::read(spec.remote.is_some() || spec.pick_remote);
        let recall = cmdline::recall::Start::read();
        // The config runs alongside everything else starting.
        let lua_start = self.process.config.clone().map(|dir| {
            let proxy = proxy();
            desk::LuaStart(Box::new(move |options| {
                let proxy = proxy.clone();
                let wake: lua::Wake = Box::new(move || {
                    let _ = proxy.send_event(SessionEvent::Lua);
                });
                lua::Lua::start(lua::Source::Dir(dir.clone()), options.clone(), wake)
            }))
        });
        let session = if spec.pick_remote {
            Session::waiting(runtime, proxy())
        } else {
            let location = match &spec.remote {
                Some(remote) => Location::Remote(remote.clone()),
                None => Location::Local(runtime.clone()),
            };
            Session::start(location, runtime, proxy())
        };
        updates::after_opening(proxy());
        let fixed = spec.font_fixed.options();
        let pick = spec.pick_remote;
        let mut app = App::new(started, Some(startup), session, spec, self.link.clone());
        // Before config runs, which starts from them.
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
        if let Some(closed) = self.on_window(event_loop, key, |app| app.resume(event_loop)) {
            return closed.and(Err(anyhow!("the window closed as it opened")));
        }
        let app = &self.windows[&key];
        if let Some(id) = app.window_id() {
            self.ids.insert(id, key);
        }
        if self.shared.is_none()
            && let Some(frame) = &app.frame
        {
            self.shared = Some(Shared {
                gpu: frame.gpu.clone(),
                fonts: frame.fonts.font_cx.collection.clone(),
                desktop: frame.desktop.clone(),
            });
        }
        self.link.0.windows.set(self.windows.len());
        Ok(())
    }

    /// Opens another window, for `spec`, with what the first made,
    /// timing its start from `started`.
    fn open_another(
        &mut self,
        event_loop: &ActiveEventLoop,
        spec: Spec,
        launch: LaunchToken,
        started: Instant,
    ) -> anyhow::Result<()> {
        let shared = self.shared.as_ref().context("no window has opened")?;
        let start = options::Options::drawing_with(&spec.font);
        let fonts =
            Fonts::for_window_in(shared.fonts.clone(), &start.font_options(), &shared.desktop)?;
        let startup = Startup {
            fonts: Pending::Ready((fonts, shared.desktop.clone())),
            gpu: Pending::Ready(shared.gpu.clone()),
            launch,
        };
        let key = self.add(spec, start, started, startup);
        self.open(event_loop, key)
    }

    /// Opens the window another process asked for, if it still waits.
    fn asked(&mut self, event_loop: &ActiveEventLoop, mut asked: Asked) {
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
        if let Err(err) = &opened {
            log::warn!("couldn't open a window asked for: {err:#}");
        }
        asked.opened(opened.map_err(|err| format!("{err:#}")));
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

    /// Starts taking windows asked for by other processes, once a window
    /// is open to share its GPU with them.
    fn listen(&mut self) {
        if self.listening.is_some()
            || self.shared.is_none()
            || !self.process.hosting
            || !handoff::on()
        {
            return;
        }
        let proxy = self.proxy.clone();
        let deliver = move |asked| drop(proxy.send_event(Wake::Asked(asked)));
        match handoff::listen(&self.process.runtime, self.process.graphics_api, deliver) {
            Ok(listening) => self.listening = Some(listening),
            Err(err) => log::warn!("windows opened later get processes of their own: {err:#}"),
        }
    }

    /// Runs `handle` on the window `key`, if it's open. If that closes it,
    /// or panics, the window closes, and this returns how it went.
    fn on_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        key: WindowKey,
        handle: impl FnOnce(&mut App),
    ) -> Option<anyhow::Result<()>> {
        let app = self.windows.get_mut(&key)?;
        match guarded(app, handle) {
            Handled::Open => None,
            Handled::Closing => Some(self.close(event_loop, key, None)),
            Handled::Panicked(why) => Some(self.close(event_loop, key, Some(why))),
        }
    }

    /// Closes the window `key`, which panicked if `panicked` says why, and
    /// returns how it went. The process ends with its last window.
    fn close(
        &mut self,
        event_loop: &ActiveEventLoop,
        key: WindowKey,
        panicked: Option<String>,
    ) -> anyhow::Result<()> {
        let Some(mut app) = self.windows.remove(&key) else {
            return Ok(());
        };
        self.ids.retain(|_, open| *open != key);
        self.wakes.remove(&key);
        self.link.0.windows.set(self.windows.len());
        // What a window that panicked holds may be half changed: nothing
        // more is asked of it.
        let result = if let Some(why) = panicked {
            log::error!("a window failed, and closes: {why}");
            Err(anyhow!("the window failed: {why}"))
        } else {
            let closed = catch_unwind(AssertUnwindSafe(|| app.closed()));
            let result = std::mem::replace(&mut app.result, Ok(()));
            result.and(closed.map_err(|panic| anyhow!("closing failed: {}", said(&*panic))))
        };
        if catch_unwind(AssertUnwindSafe(move || drop(app))).is_err() {
            log::error!("a window failed as it closed");
        }
        if self.windows.is_empty() {
            if let Err(err) = &result {
                self.result = Err(anyhow!("{err:#}"));
            }
            event_loop.exit();
        } else if let Err(err) = &result {
            log::error!("{err:#}");
        }
        result
    }
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
        let waiting: Vec<WindowKey> = self.windows.keys().copied().collect();
        for key in waiting {
            // Its failure is the process's, if it's the last window.
            let _ = self.open(event_loop, key);
        }
        self.listen();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(&key) = self.ids.get(&id) else {
            return;
        };
        self.on_window(event_loop, key, |app| {
            app.handle_window_event(event);
            app.send_outbox();
        });
        self.open_asked(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, wake: Wake) {
        match wake {
            // A window that closed as it connected: its session thread
            // waits on the connection until it's hung up.
            Wake::Window(key, SessionEvent::Connected { sender, .. })
                if !self.windows.contains_key(&key) =>
            {
                sender.close();
            }
            Wake::Window(key, event) => {
                self.on_window(event_loop, key, |app| {
                    // Every workspace has closed.
                    if app.session_event(event) {
                        app.closing = true;
                    }
                });
            }
            Wake::Asked(asked) => self.asked(event_loop, asked),
        }
        self.open_asked(event_loop);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            let now = Instant::now();
            let due: Vec<WindowKey> = (self.wakes.iter())
                .filter(|(_, wake)| **wake <= now)
                .map(|(key, _)| *key)
                .collect();
            for key in due {
                self.on_window(event_loop, key, |app| app.woke(now));
            }
            self.open_asked(event_loop);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let keys: Vec<WindowKey> = self.windows.keys().copied().collect();
        for key in keys {
            let mut wake = None;
            self.on_window(event_loop, key, |app| wake = app.idle());
            match wake {
                Some(wake) => self.wakes.insert(key, wake),
                None => self.wakes.remove(&key),
            };
        }
        self.open_asked(event_loop);
        let flow = match self.wakes.values().min() {
            Some(wake) => ControlFlow::WaitUntil(*wake),
            None => ControlFlow::Wait,
        };
        event_loop.set_control_flow(flow);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Others stop asking this process for windows first.
        self.listening = None;
        for app in self.windows.values_mut() {
            if catch_unwind(AssertUnwindSafe(|| app.closed())).is_err() {
                log::error!("a window failed as it closed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let runtime = RuntimeDir::at(PathBuf::from("/unused-host-test"));
        let session = Session::waiting(&runtime, Proxy::Channel(std::sync::mpsc::channel().0));
        App::new(
            Instant::now(),
            None,
            session,
            Spec::default(),
            Link::default(),
        )
    }

    #[test]
    fn a_window_that_panics_is_closed_saying_why_and_one_that_asks_is_closed() {
        let mut app = app();
        assert_eq!(guarded(&mut app, |_| {}), Handled::Open);
        let panicked = guarded(&mut app, |_| panic!("a bug in one window"));
        assert_eq!(panicked, Handled::Panicked("a bug in one window".into()));
        let panicked = guarded(&mut app, |_| panic!("{} bugs", 2));
        assert_eq!(panicked, Handled::Panicked("2 bugs".into()));
        assert_eq!(
            guarded(&mut app, |app| app.closing = true),
            Handled::Closing
        );
    }

    #[test]
    fn a_window_alone_in_its_process_opens_others_in_processes_of_their_own() {
        let link = Link::default();
        assert!(!link.hosting());
        assert!(link.alone());
        assert!(!link.replaced(Path::new("/proc/self/exe")));
        let hosting = Link(Rc::new(Linked {
            build: Build::running(),
            ..Linked::default()
        }));
        assert!(hosting.hosting());
        assert!(!hosting.replaced(Path::new("/proc/self/exe")));
        assert!(hosting.replaced(Path::new("/nonexistent/clackatoo")));
    }
}
