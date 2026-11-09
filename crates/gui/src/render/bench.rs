//! A terminal's frame built as the window builds one, for the end-to-end
//! harness's keystroke gate (CLACKA-241): the rectangles and glyphs of a
//! whole screen of text, the work on the CPU each frame costs the window
//! once a key's echo arrives, without drawing them.

use clackatoo_protocol::View;
use clackatoo_term::{Config, Size, Terminal};

use super::{CursorStyle, Picture, Renderer, Scene, SceneCursor};
use crate::fonts::{FontOptions, Fonts};
use crate::screen::Screen;

/// A renderer with two screens of a terminal to build frames of in turn,
/// one with a key typed at its top left and one with another, as typing
/// changes a screen between frames.
pub struct FrameBench {
    renderer: Renderer,
    fonts: Fonts,
    screens: [Screen; 2],
    /// Which screen the next frame shows.
    next: usize,
}

impl std::fmt::Debug for FrameBench {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameBench")
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl FrameBench {
    /// Frames of a terminal `cols` by `rows` cells that `output` was
    /// played into, in the system's monospace font. `None` without a GPU
    /// to make a renderer on, or a font.
    #[must_use]
    pub fn new(cols: u16, rows: u16, output: &[u8]) -> Option<Self> {
        let (device, _queue) = crate::app::look::shared_device()?;
        let fonts = Fonts::load(&FontOptions::default(), 1.0).ok()?;
        let size = Size::new(cols, rows).ok()?;
        let screen = |key: &[u8]| {
            let mut terminal = Terminal::new(size, &Config::default());
            terminal.advance(output);
            terminal.advance(b"\x1b[H");
            terminal.advance(key);
            Screen::new(&View::capture(&terminal))
        };
        let screens = [screen(b"a")?, screen(b"b")?];
        let renderer = Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        Some(Self {
            renderer,
            fonts,
            screens,
            next: 0,
        })
    }

    /// Builds the next frame's rectangles and glyphs.
    pub fn frame(&mut self) {
        let screen = &self.screens[self.next];
        self.next = 1 - self.next;
        let rows: Vec<&clackatoo_term::Row> = screen.rows.iter().collect();
        let cursor = screen.state.cursor;
        #[allow(clippy::cast_precision_loss, reason = "a screen's pixels are few")]
        let size = [
            f32::from(screen.size.cols()) * self.fonts.cell.width as f32,
            f32::from(screen.size.rows()) * self.fonts.cell.height as f32,
        ];
        let scene = Scene {
            scroll: None,
            origin: [0.0; 2],
            size,
            rows: &rows,
            cursor: Some(SceneCursor {
                line: usize::from(cursor.line),
                col: usize::from(cursor.col),
                style: CursorStyle::Block,
            }),
            normal_mode: false,
            highlights: &[],
        };
        let picture = Picture {
            scenes: &[scene],
            fills: &[],
            over: &[],
            texts: &[],
            glyphs: &[],
            over_glyphs: &[],
            images: &[],
            transparent: false,
        };
        self.renderer.build(&picture, &mut self.fonts);
    }
}
