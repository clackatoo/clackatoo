//! clackatoo's window: draws the daemon's workspaces, their tabs and their
//! panes' terminals with the GPU, kept current by the daemon's updates.
//!
//! The window is a client of the daemon like any other. It watches the
//! workspaces, attaches to the terminals of the one it shows, mirrors each
//! terminal's view in a [`Screen`](screen::Screen), and redraws only when
//! an update arrives, acknowledging each once it's on screen so the daemon
//! sends the next. Between updates it does nothing at all.
//!
//! Starting up overlaps the slow parts: fonts load and the GPU opens on
//! threads of their own while the event loop starts, and a third thread
//! connects to the daemon, on this machine or, over ssh, another. Or the
//! window opens as the picker of a machine to connect to (see
//! [`connect`]), and connects once one is picked.

#![forbid(unsafe_code)]

mod app;
mod board;
mod boxdraw;
mod browser;
mod chat;
mod chrome;
mod clipboard_image;
mod cmdline;
mod colors;
mod command;
mod complete;
mod connect;
mod desk;
mod desktop;
mod editor;
mod explorer;
mod finder;
mod fonts;
mod form;
mod forwards;
mod glyphs;
mod handoff;
mod history;
mod hlsearch;
pub mod hosts;
mod ime;
mod input;
mod keymap;
mod launch;
mod layout;
mod links;
mod lua;
mod markdown;
mod motion;
mod notify;
mod objects;
mod options;
mod other;
mod pane;
mod picker;
mod render;
mod review;
mod screen;
mod scroll;
mod session;
mod shortcuts;
mod smooth;
mod start;
mod tutor;
mod updates;
mod vim;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
#[doc(hidden)]
pub use app::look;
use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::remote::Remote;
pub use complete::Ranking;
pub use editor::EditorBench;
pub use fonts::FontOptions;
pub use launch::LaunchToken;
use winit::event_loop::EventLoop;

/// How to open the window.
#[derive(Clone, Debug)]
pub struct Options {
    /// Where the daemon is, or will be started.
    pub runtime: RuntimeDir,
    /// The other machine whose daemon to use instead, if any.
    pub remote: Option<Remote>,
    /// Whether to open as the picker of a machine to connect to, as
    /// `remote` would, rather than connect anywhere first.
    pub pick_remote: bool,
    /// The shell command that runs clackatoo on another machine, for one
    /// picked.
    pub remote_command: String,
    /// What to draw text with, until config says otherwise.
    pub font: FontOptions,
    /// Which of `font`'s settings the command line gave, or the
    /// environment, which config doesn't change.
    pub font_fixed: FontFixed,
    /// The graphics API to draw with.
    pub graphics_api: GraphicsApi,
    /// clackatoo's config directory, whose `client/init.lua` configures
    /// the window, if there is one.
    pub config: Option<PathBuf>,
}

/// Which of the font's settings were given on the command line, or in the
/// environment, and so outdo config's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontFixed {
    /// The family, `--font`.
    pub family: bool,
    /// The size, `--font-size`.
    pub size: bool,
}

impl FontFixed {
    /// The options these are.
    fn options(self) -> Vec<&'static options::Info> {
        let named = |fixed: bool, name| fixed.then(|| options::find(name).expect("an option"));
        named(self.family, "font")
            .into_iter()
            .chain(named(self.size, "fontsize"))
            .collect()
    }
}

/// The graphics API the window draws with. If the one chosen can't draw,
/// the window tries the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GraphicsApi {
    /// Vulkan, which is fastest where it works well.
    #[default]
    Vulkan,
    /// OpenGL, which on some machines with two GPUs opens a window much
    /// sooner: Vulkan asks both GPUs about themselves first, which can
    /// take seconds.
    OpenGl,
}

/// Takes the token the desktop launched this process with out of its
/// environment, as a window does as it starts, so nothing it starts
/// inherits it. For the daemon: one started from a launcher, or by a
/// window from before windows took it, has it still, and keeps it through
/// every upgrade, and its shells would inherit a token long spent.
///
/// Changing the environment is only safe while no other thread could be
/// reading it, so this must come before the process starts any thread.
pub fn forget_launch_token() {
    launch::LaunchToken::take();
}

/// Takes the desktop's launch token out of the environment, as
/// [`forget_launch_token`] does, for a command that hands it on: to a
/// window already open, or as the environment of one it starts.
///
/// It must come before the process starts any thread, or any program,
/// such as the daemon.
#[must_use]
pub fn take_launch_token() -> LaunchToken {
    launch::LaunchToken::take()
}

/// Opens a window on the daemon's workspaces, making one if there are none,
/// and returns once it's open.
///
/// The window opens in the process of windows, which shares its GPU with
/// every window, so that only the first sets it up: one already running is
/// asked for it, or else this process forks one, apart from itself and its
/// terminal, and hands it the window (see [`handoff`]). Whatever the window
/// starts, the daemon and ssh among them, is started with this process's
/// environment and directory. Only if no process of windows can open it
/// does the window open in this one, which then runs until it closes; with
/// `CLACKATOO_WINDOW_HOST=off`, it always does.
///
/// It takes the desktop's launch token out of the environment first, and
/// may fork, so it must be called before this process starts any thread.
///
/// # Errors
///
/// If the window can't open, no GPU or font is usable, or the connection to
/// the daemon fails.
pub fn run(options: Options) -> anyhow::Result<()> {
    let started = Instant::now();
    // First, while this is the only thread: nothing started from here on
    // inherits the desktop's token, the daemon included.
    let launch = launch::LaunchToken::take();
    if launch.token().is_some() {
        log::debug!("launched with the desktop's activation token");
    }
    let mut spec = app::host::Spec {
        remote: options.remote.clone(),
        pick_remote: options.pick_remote,
        remote_command: options.remote_command.clone(),
        font: options.font.clone(),
        font_fixed: options.font_fixed,
        origin: clackatoo_daemon::launch::Origin::default(),
    };
    if !handoff::on() {
        return open_here(options, spec, launch, started, false);
    }
    spec.origin = handoff::here();
    let request = handoff::Request {
        spec: &spec,
        graphics_api: options.graphics_api,
        runtime: &options.runtime,
        waits: handoff::Waits::default(),
    };
    let opened = |how: &str| {
        log::info!("window opened by {how}, after {:.1?}", started.elapsed());
    };
    let not = match handoff::ask(&request, launch, None) {
        Ok(()) => {
            opened("the process of windows");
            return Ok(());
        }
        Err(not) => not,
    };
    let mut launch = not.launch;
    if not.then != handoff::Then::Here {
        log::debug!("forking a process of windows: {}", not.why);
        let listen = not.then == handoff::Then::Host;
        match fork_host(&options, listen) {
            Ok(forked) => match handoff::ask(&request, launch, Some(forked)) {
                Ok(()) => {
                    opened("a process of windows forked for it");
                    return Ok(());
                }
                Err(not) => {
                    log::warn!(
                        "the process of windows forked didn't open the window: {}",
                        not.why
                    );
                    launch = not.launch;
                }
            },
            Err(err) => log::warn!("couldn't fork a process of windows: {err:#}"),
        }
    }
    spec.origin = clackatoo_daemon::launch::Origin::default();
    open_here(options, spec, launch, started, true)
}

/// Opens the window for `spec` in this process, which runs until it's
/// closed; windows it opens itself, with `share`, open here too.
fn open_here(
    options: Options,
    spec: app::host::Spec,
    launch: launch::LaunchToken,
    started: Instant,
    share: bool,
) -> anyhow::Result<()> {
    let start = options::Options::drawing_with(&spec.font);
    let (event_loop, fonts, gpu) = starting(&start.font_options(), options.graphics_api)?;
    let process = app::host::Process {
        runtime: options.runtime,
        config: options.config,
        graphics_api: options.graphics_api,
        listen: false,
        share,
    };
    let waker = app::host::Waker::EventLoop(event_loop.create_proxy());
    let mut host = app::host::Host::new(process, waker);
    let startup = app::Startup { fonts, gpu, launch };
    host.add(spec, start, started, startup);
    event_loop.run_app(&mut host)?;
    host.result
}

/// Forks a process of windows, apart from this one (see
/// [`clackatoo_daemon::launch::fork_apart`]), taking the windows asked for
/// on `window.sock` too, with `listen`. Returns this end of the socket pair
/// its first window is to be asked for on. The process forked never
/// returns: it runs until its last window closes, then exits.
fn fork_host(options: &Options, listen: bool) -> anyhow::Result<std::os::unix::net::UnixStream> {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    match clackatoo_daemon::launch::fork_apart()? {
        clackatoo_daemon::launch::Forked::Parent(_) => Ok(ours),
        clackatoo_daemon::launch::Forked::Child => {
            drop(ours);
            let hosted = host(
                options.runtime.clone(),
                options.config.clone(),
                options.graphics_api,
                listen,
                theirs,
            );
            let code = match hosted {
                Ok(()) => 0,
                Err(err) => {
                    log::error!("the process of windows failed: {err:#}");
                    1
                }
            };
            std::process::exit(code);
        }
    }
}

/// Runs the process of windows, forked by `clackatoo` (see [`run`]), with
/// the config in `config` and drawing with `graphics_api`: it reads the
/// first window asked for on `first`, sets up the GPU and finds that
/// window's fonts at once, takes the windows asked for on `window.sock`
/// in `runtime` too, with `listen`, and ends with its last window.
fn host(
    runtime: RuntimeDir,
    config: Option<PathBuf>,
    graphics_api: GraphicsApi,
    listen: bool,
    first: std::os::unix::net::UnixStream,
) -> anyhow::Result<()> {
    // Panics are logged, as it has no stderr to say them on.
    let said = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log::error!("{panic}");
        said(panic);
    }));
    let first = handoff::first(first, graphics_api)?;
    let start = options::Options::drawing_with(&first.spec.font);
    let (event_loop, fonts, gpu) = starting(&start.font_options(), graphics_api)?;
    let process = app::host::Process {
        runtime,
        config,
        graphics_api,
        listen,
        share: true,
    };
    let waker = app::host::Waker::EventLoop(event_loop.create_proxy());
    let mut host = app::host::Host::new(process, waker);
    host.starting(fonts, gpu, first);
    // Before the event loop runs, so windows asked for meanwhile wait for
    // this process rather than fork their own.
    host.listen();
    event_loop.run_app(&mut host)?;
    host.result
}

/// The event loop, and the fonts and GPU being made for the first window.
type Starting = (
    EventLoop<app::host::Wake>,
    app::Pending<(fonts::Fonts, desktop::Desktop)>,
    app::Pending<app::Gpu>,
);

/// Starts finding the fonts in `font`, and setting up the GPU with
/// `graphics_api`, on threads of their own, and makes the event loop
/// meanwhile.
fn starting(font: &FontOptions, graphics_api: GraphicsApi) -> anyhow::Result<Starting> {
    let fonts = {
        let font = font.clone();
        std::thread::Builder::new()
            .name("fonts".into())
            .spawn(move || {
                // Asking the desktop takes a few milliseconds, which the
                // GPU takes longer than anyway.
                let desktop = desktop::Desktop::read();
                let fonts = fonts::Fonts::for_window(&font, &desktop)?;
                // The chat's fonts are found from now, alongside the GPU,
                // so a chat on screen as the window opens doesn't wait.
                chat::warm_fonts(&fonts);
                Ok((fonts, desktop))
            })?
    };
    let (display, display_received) = std::sync::mpsc::channel();
    let gpu = std::thread::Builder::new()
        .name("gpu".into())
        .spawn(move || app::Gpu::new(graphics_api, &display_received))?;
    let event_loop = EventLoop::<app::host::Wake>::with_user_event()
        .build()
        .context("could not open a window")?;
    // The GPU thread has finished without it if this fails.
    let _ = display.send(event_loop.owned_display_handle());
    Ok((
        event_loop,
        app::Pending::Thread(fonts),
        app::Pending::Thread(gpu),
    ))
}
