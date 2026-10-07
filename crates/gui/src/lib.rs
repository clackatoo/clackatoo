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
//! connects to the daemon.

#![forbid(unsafe_code)]

mod app;
mod colors;
mod fonts;
mod glyphs;
mod input;
mod render;
mod screen;
mod session;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
pub use fonts::FontOptions;
use winit::event_loop::EventLoop;

/// How to open the window.
#[derive(Clone, Debug)]
pub struct Options {
    /// The daemon's socket.
    pub socket: PathBuf,
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
    let gpu = std::thread::Builder::new()
        .name("gpu".into())
        .spawn(app::Gpu::new)?;
    let event_loop = EventLoop::with_user_event()
        .build()
        .context("could not open a window")?;
    session::spawn(options.socket, event_loop.create_proxy());
    let mut app = app::App::new(started, fonts, gpu);
    event_loop.run_app(&mut app)?;
    app.result
}
