//! clackatoo's window: draws a terminal's view with the GPU, kept current
//! by the daemon's updates.
//!
//! The window is a client of the daemon like any other. It attaches to a
//! terminal, mirrors its view in a [`Screen`](screen::Screen), and redraws
//! only when an update arrives, acknowledging each once it's on screen so
//! the daemon sends the next. Between updates it does nothing at all.
//!
//! Starting up overlaps the slow parts: fonts load and the GPU opens on
//! threads of their own while the event loop starts, and a third thread
//! connects to the daemon, on this machine or, over ssh, another.

#![forbid(unsafe_code)]

mod app;
mod boxdraw;
mod colors;
mod fonts;
mod glyphs;
mod history;
mod input;
mod motion;
mod pane;
mod render;
mod screen;
mod session;

#[cfg(test)]
mod tests;

use std::time::Instant;

use anyhow::Context;
use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::remote::Remote;
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
}

/// Opens a window on the newest running terminal, or a new one, and runs
/// until the window closes.
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
            .spawn(move || fonts::Fonts::load(&options, 1.0))?
    };
    let (display, display_received) = std::sync::mpsc::channel();
    let gpu = std::thread::Builder::new()
        .name("gpu".into())
        .spawn(move || app::Gpu::new(&display_received))?;
    let event_loop = EventLoop::with_user_event()
        .build()
        .context("could not open a window")?;
    // The GPU thread has finished without it if this fails.
    let _ = display.send(event_loop.owned_display_handle());
    let location = match options.remote {
        Some(remote) => session::Location::Remote(remote),
        None => session::Location::Local(options.runtime.clone()),
    };
    let session = session::Session::start(location, &options.runtime, event_loop.create_proxy());
    let mut app = app::App::new(started, fonts, gpu, session);
    event_loop.run_app(&mut app)?;
    app.result
}
