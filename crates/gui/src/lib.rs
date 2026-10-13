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
//! connects to the daemon, on this machine or, over ssh, another.

#![forbid(unsafe_code)]

mod app;
mod board;
mod boxdraw;
mod chat;
mod chrome;
mod colors;
mod command;
mod desk;
mod desktop;
mod editor;
mod explorer;
mod finder;
mod fonts;
mod glyphs;
mod history;
mod ime;
mod input;
mod keymap;
mod layout;
mod links;
mod lua;
mod motion;
mod objects;
mod options;
mod pane;
mod render;
mod review;
mod screen;
mod session;
mod tutor;
mod updates;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::remote::Remote;
pub use editor::Ranking;
pub use fonts::FontOptions;
use winit::event_loop::EventLoop;

/// How to open the window.
#[derive(Clone, Debug)]
pub struct Options {
    /// Where the daemon is, or will be started.
    pub runtime: RuntimeDir,
    /// The other machine whose daemon to use instead, if any.
    pub remote: Option<Remote>,
    /// What to draw text with.
    pub font: FontOptions,
    /// The graphics API to draw with.
    pub graphics_api: GraphicsApi,
    /// clackatoo's config directory, whose `client/init.lua` configures
    /// the window, if there is one.
    pub config: Option<PathBuf>,
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

/// Opens a window on the daemon's workspaces, making one if there are none,
/// and runs until the window closes or every workspace has.
///
/// # Errors
///
/// If the window can't open, no GPU or font is usable, or the connection to
/// the daemon fails.
pub fn run(options: Options) -> anyhow::Result<()> {
    let started = Instant::now();
    let fonts = {
        let options = options.font.clone();
        std::thread::Builder::new()
            .name("fonts".into())
            .spawn(move || {
                // Asking the desktop takes a few milliseconds, which the
                // GPU takes longer than anyway.
                let desktop = desktop::Desktop::read();
                let mut fonts = fonts::Fonts::load(&options, 1.0)?;
                let (family, points) = match &desktop.font {
                    Some((family, points)) => (Some(family.as_str()), *points),
                    None => (None, 11.0),
                };
                fonts.load_ui(family, points, 1.0);
                Ok((fonts, desktop))
            })?
    };
    let (display, display_received) = std::sync::mpsc::channel();
    let graphics_api = options.graphics_api;
    let gpu = std::thread::Builder::new()
        .name("gpu".into())
        .spawn(move || app::Gpu::new(graphics_api, &display_received))?;
    let event_loop = EventLoop::with_user_event()
        .build()
        .context("could not open a window")?;
    // The GPU thread has finished without it if this fails.
    let _ = display.send(event_loop.owned_display_handle());
    // A small file read, and a directory or so looked at.
    let tutor = tutor::Start::read(options.remote.is_some());
    let location = match options.remote {
        Some(remote) => session::Location::Remote(remote),
        None => session::Location::Local(options.runtime.clone()),
    };
    // The config runs alongside everything else starting.
    let lua_start = options.config.map(|dir| {
        let proxy = event_loop.create_proxy();
        desk::LuaStart(Box::new(move || {
            let proxy = proxy.clone();
            let wake: lua::Wake = Box::new(move || {
                let _ = proxy.send_event(session::SessionEvent::Lua);
            });
            lua::Lua::start(lua::Source::Dir(dir.clone()), wake)
        }))
    });
    let session = session::Session::start(location, &options.runtime, event_loop.create_proxy());
    updates::after_opening(event_loop.create_proxy());
    let mut app = app::App::new(started, fonts, gpu, session);
    if let Some(start) = lua_start {
        app.start_lua(start);
    }
    app.start_tutor(tutor);
    event_loop.run_app(&mut app)?;
    app.result
}
