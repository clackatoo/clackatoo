//! The window: starting up, and turning window and daemon events into
//! frames.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, anyhow};
use clackatoo_daemon::Sender;
use clackatoo_protocol::{Event, ServerMessage, Size};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::Key;
use winit::window::{Window, WindowAttributes, WindowId};

use crate::fonts::Fonts;
use crate::input;
use crate::pane::{Mode, Pane};
use crate::render::{CursorStyle, PADDING, Renderer, Scene};
use crate::session::SessionEvent;

/// Lines scrolled per notch of a mouse wheel.
const WHEEL_LINES: f64 = 3.0;

/// The grid a new window is sized for.
const INITIAL_GRID: (u32, u32) = (100, 30);

/// A GPU device, made on its own thread while the window opens.
#[derive(Debug)]
pub(crate) struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Gpu {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        // The window doesn't exist yet, so the adapter can't be chosen by
        // the screen it draws on. On desktops with two GPUs, the discrete
        // one usually drives the screen; the integrated one may not be able
        // to hand frames to the compositor at all. If the guess is wrong,
        // the window picks again (see `App::open`).
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no GPU found")?;
        Self::open(instance, adapter)
    }

    fn open(instance: wgpu::Instance, adapter: wgpu::Adapter) -> anyhow::Result<Self> {
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("clackatoo"),
            ..Default::default()
        }))
        .context("could not open the GPU")?;
        // A GPU error is a bug in how we drive it, but the window can often
        // carry on, so it's logged rather than fatal.
        device.on_uncaptured_error(Arc::new(|error| log::error!("GPU error: {error}")));
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Sets `surface` up to show frames, or says why it can't.
    fn configure(
        &self,
        surface: &wgpu::Surface<'_>,
        size: PhysicalSize<u32>,
    ) -> anyhow::Result<wgpu::SurfaceConfiguration> {
        if !self.adapter.is_surface_supported(surface) {
            anyhow::bail!("{} can't draw to this window", self.adapter.get_info().name);
        }
        let capabilities = surface.get_capabilities(&self.adapter);
        // Colours are blended in sRGB, as other terminals do, so text has
        // its usual weight. That needs a format that doesn't convert.
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .or_else(|| capabilities.formats.first().copied())
            .context("the window supports no texture format")?;
        let present_mode = if capabilities
            .present_modes
            .contains(&wgpu::PresentMode::Mailbox)
        {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            desired_maximum_frame_latency: 2,
            present_mode,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        // Some GPUs claim support and then fail here, so the result is
        // checked rather than trusted.
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        surface.configure(&self.device, &config);
        if let Some(error) = pollster::block_on(scope.pop()) {
            anyhow::bail!(
                "{} can't draw to this window: {error}",
                self.adapter.get_info().name
            );
        }
        Ok(config)
    }
}

/// Work started before the window opened.
#[derive(Debug)]
struct Startup {
    fonts: JoinHandle<anyhow::Result<Fonts>>,
    gpu: JoinHandle<anyhow::Result<Gpu>>,
}

/// The open window and what draws into it.
#[derive(Debug)]
struct Frame {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    renderer: Renderer,
    fonts: Fonts,
}

/// The window's whole state.
#[derive(Debug)]
pub(crate) struct App {
    started: Instant,
    startup: Option<Startup>,
    frame: Option<Frame>,
    /// The terminal shown, once attached.
    pane: Option<Pane>,
    /// Where messages to the daemon go, once attached.
    sender: Option<Sender>,
    /// The modifier keys held now.
    modifiers: input::Modifiers,
    focused: bool,
    /// Scrolling from touchpads arrives in pixels; this is what's left
    /// over after whole lines, in lines.
    wheel_remainder: f64,
    /// Opened on the first paste.
    clipboard: Option<Clipboard>,
    first_frame_drawn: bool,
    /// Why the window closed, if something went wrong.
    pub(crate) result: anyhow::Result<()>,
}

impl App {
    pub(crate) fn new(
        started: Instant,
        fonts: JoinHandle<anyhow::Result<Fonts>>,
        gpu: JoinHandle<anyhow::Result<Gpu>>,
    ) -> Self {
        Self {
            started,
            startup: Some(Startup { fonts, gpu }),
            frame: None,
            pane: None,
            sender: None,
            modifiers: input::Modifiers::default(),
            focused: true,
            wheel_remainder: 0.0,
            clipboard: None,
            first_frame_drawn: false,
            result: Ok(()),
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.result = Err(error);
        event_loop.exit();
    }

    /// Opens the window once fonts and the GPU are ready.
    fn open(&mut self, event_loop: &ActiveEventLoop, startup: Startup) -> anyhow::Result<Frame> {
        let join = |name| move |_| anyhow!("{name} thread panicked");
        let mut fonts = startup.fonts.join().map_err(join("font"))??;
        let fonts_ready = self.started.elapsed();
        // Fonts were loaded at one pixel per point; the window's display
        // may have more.
        let (cols, rows) = INITIAL_GRID;
        let padding = 2.0 * PADDING;
        let size = LogicalSize::new(
            f64::from(cols * fonts.cell.width) + padding,
            f64::from(rows * fonts.cell.height) + padding,
        );
        let window = Arc::new(event_loop.create_window(window_attributes(size))?);
        fonts.set_scale(window.scale_factor());

        let mut gpu = startup.gpu.join().map_err(join("GPU"))??;
        let gpu_ready = self.started.elapsed();
        let surface = gpu.instance.create_surface(Arc::clone(&window))?;
        let config = match gpu.configure(&surface, window.inner_size()) {
            Ok(config) => config,
            Err(err) => {
                // The guessed GPU can't show this window: ask for one that
                // can, now that there's a window to ask about.
                log::warn!("{err:#}; looking for another GPU");
                let adapter = pollster::block_on(gpu.instance.request_adapter(
                    &wgpu::RequestAdapterOptions {
                        compatible_surface: Some(&surface),
                        ..Default::default()
                    },
                ))
                .context("no GPU can draw to this window")?;
                gpu = Gpu::open(gpu.instance.clone(), adapter)?;
                gpu.configure(&surface, window.inner_size())?
            }
        };
        let (format, present_mode) = (config.format, config.present_mode);
        let renderer = Renderer::new(&gpu.device, format);
        log::info!(
            "window open after {:.1?} (fonts ready after {fonts_ready:.1?}, GPU after \
             {gpu_ready:.1?}): {} adapter, {format:?}, {present_mode:?}",
            self.started.elapsed(),
            gpu.adapter.get_info().name,
        );
        Ok(Frame {
            window,
            surface,
            config,
            gpu,
            renderer,
            fonts,
        })
    }

    /// Asks the daemon to make the terminal as big as fits the window.
    fn fit_terminal(&mut self) {
        let (Some(frame), Some(pane)) = (&self.frame, &mut self.pane) else {
            return;
        };
        let cell = frame.fonts.cell;
        let padding = padding(&frame.window);
        let grid = |pixels: u32, cell: u32| {
            let cells = (pixels.saturating_sub(2 * padding) / cell).max(1);
            u16::try_from(cells).unwrap_or(u16::MAX)
        };
        let Ok(size) = Size::new(
            grid(frame.config.width, cell.width),
            grid(frame.config.height, cell.height),
        ) else {
            return;
        };
        pane.resize(size);
    }

    /// Handles a key press: the window's own keys, then the pane's.
    fn key_pressed(&mut self, event: &winit::event::KeyEvent) {
        let Some(pane) = &mut self.pane else {
            return;
        };
        let modifiers = self.modifiers;
        let is_paste = modifiers.ctrl
            && modifiers.shift
            && matches!(&event.logical_key, Key::Character(text) if text.eq_ignore_ascii_case("v"));
        let redraw = if is_paste {
            match paste_text(&mut self.clipboard) {
                Ok(text) => pane.paste(&text),
                Err(err) => log::warn!("pasting failed: {err:#}"),
            }
            true
        } else {
            pane.key(&input::KeyPress {
                key: &event.logical_key,
                text: event.text.as_deref(),
                location: event.location,
                modifiers,
            })
        };
        if redraw {
            self.request_redraw();
        }
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        let (Some(frame), Some(pane)) = (&self.frame, &mut self.pane) else {
            return;
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * WHEEL_LINES,
            MouseScrollDelta::PixelDelta(position) => {
                position.y / f64::from(frame.fonts.cell.height)
            }
        } + self.wheel_remainder;
        let whole = lines.trunc();
        self.wheel_remainder = lines - whole;
        #[allow(clippy::cast_possible_truncation)]
        if whole != 0.0 && pane.wheel(whole as i64) {
            frame.window.request_redraw();
        }
    }

    /// Sends what the pane has for the daemon.
    fn send_outbox(&mut self) {
        let (Some(pane), Some(sender)) = (&mut self.pane, &mut self.sender) else {
            return;
        };
        for message in pane.take_outbox() {
            if let Err(err) = sender.send(&message) {
                log::warn!("sending to the daemon failed: {err}");
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(frame) = &self.frame {
            frame.window.request_redraw();
        }
    }

    fn draw(&mut self) {
        let (Some(frame), Some(pane)) = (&mut self.frame, &mut self.pane) else {
            return;
        };
        let Some(screen) = pane.screen() else {
            return;
        };
        let texture = match frame.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                frame.surface.configure(&frame.gpu.device, &frame.config);
                frame.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("the GPU refused a frame");
                return;
            }
        };
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        #[allow(clippy::cast_precision_loss)]
        let padding = padding(&frame.window) as f32;
        let (rows, mut cursor) = pane.scene(screen.rows.len());
        if let Some(cursor) = &mut cursor
            && !self.focused
        {
            cursor.style = CursorStyle::Hollow;
        }
        let label = pane.label();
        let highlights = pane.highlights(rows.len());
        let scene = Scene {
            rows: &rows,
            cursor,
            normal_mode: pane.mode != Mode::Insert,
            label: label.as_deref(),
            highlights: &highlights,
        };
        frame.renderer.render(
            (&frame.gpu.device, &frame.gpu.queue),
            &view,
            [frame.config.width, frame.config.height],
            padding,
            &scene,
            &mut frame.fonts,
        );
        frame.window.pre_present_notify();
        frame.gpu.queue.present(texture);

        if !self.first_frame_drawn {
            self.first_frame_drawn = true;
            log::info!("first frame after {:.1?}", self.started.elapsed());
        }
        pane.drawn();
    }
}

/// The new window's title, size and app ID. Desktops find the window's
/// icon through the app ID, matching it to `clackatoo.desktop`.
fn window_attributes(size: LogicalSize<f64>) -> WindowAttributes {
    let attributes = Window::default_attributes()
        .with_title("clackatoo")
        .with_inner_size(size);
    #[cfg(target_os = "linux")]
    let attributes = {
        use winit::platform::wayland::WindowAttributesExtWayland;
        use winit::platform::x11::WindowAttributesExtX11;
        let attributes =
            WindowAttributesExtWayland::with_name(attributes, "clackatoo", "clackatoo");
        WindowAttributesExtX11::with_name(attributes, "clackatoo", "clackatoo")
    };
    attributes
}

/// The system clipboard.
struct Clipboard(arboard::Clipboard);

impl std::fmt::Debug for Clipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clipboard")
    }
}

/// The clipboard, opened on first use.
fn clipboard(slot: &mut Option<Clipboard>) -> anyhow::Result<&mut arboard::Clipboard> {
    let clipboard = match slot {
        Some(clipboard) => clipboard,
        None => slot.insert(Clipboard(
            arboard::Clipboard::new().context("no clipboard")?,
        )),
    };
    Ok(&mut clipboard.0)
}

/// The clipboard's text.
fn paste_text(slot: &mut Option<Clipboard>) -> anyhow::Result<String> {
    Ok(clipboard(slot)?.get_text()?)
}

/// Puts `text` on the clipboard.
fn copy_text(slot: &mut Option<Clipboard>, text: String) -> anyhow::Result<()> {
    Ok(clipboard(slot)?.set_text(text)?)
}

/// The padding around the grid in physical pixels.
fn padding(window: &Window) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let padding = (PADDING * window.scale_factor()).round() as u32;
    padding
}

impl ApplicationHandler<SessionEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let Some(startup) = self.startup.take() else {
            return;
        };
        match self.open(event_loop, startup) {
            Ok(frame) => {
                self.frame = Some(frame);
                self.fit_terminal();
            }
            Err(err) => self.fail(event_loop, err),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        self.handle_window_event(event_loop, event);
        self.send_outbox();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: SessionEvent) {
        self.handle_session_event(event_loop, event);
        self.send_outbox();
    }
}

impl App {
    fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(frame) = &mut self.frame
                    && size.width > 0
                    && size.height > 0
                {
                    frame.config.width = size.width;
                    frame.config.height = size.height;
                    frame.surface.configure(&frame.gpu.device, &frame.config);
                    frame.window.request_redraw();
                }
                self.fit_terminal();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(frame) = &mut self.frame {
                    frame.fonts.set_scale(scale_factor);
                    frame.renderer.forget_glyphs();
                    frame.window.request_redraw();
                }
                self.fit_terminal();
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                self.modifiers = input::Modifiers {
                    shift: state.shift_key(),
                    ctrl: state.control_key(),
                    alt: state.alt_key(),
                };
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state.is_pressed() => self.key_pressed(&event),
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn handle_session_event(&mut self, event_loop: &ActiveEventLoop, event: SessionEvent) {
        match event {
            SessionEvent::Attached { sender, terminal } => {
                self.pane = Some(Pane::new(terminal));
                self.sender = Some(sender);
                self.fit_terminal();
            }
            SessionEvent::Message(ServerMessage::Event(Event::View { update, .. })) => {
                let Some(pane) = &mut self.pane else {
                    return;
                };
                if !pane.view_changed(&update) {
                    return;
                }
                if let (Some(frame), Some(screen)) = (&self.frame, pane.screen()) {
                    let title = if screen.title.is_empty() {
                        "clackatoo"
                    } else {
                        &screen.title
                    };
                    frame.window.set_title(title);
                    frame.window.request_redraw();
                }
            }
            SessionEvent::Message(ServerMessage::Response(response)) => {
                let Some(pane) = &mut self.pane else {
                    return;
                };
                let redraw = pane.answered(response);
                if let Some(text) = pane.take_yanked()
                    && let Err(err) = copy_text(&mut self.clipboard, text)
                {
                    log::warn!("copying failed: {err:#}");
                }
                if redraw {
                    self.request_redraw();
                }
            }
            SessionEvent::Message(ServerMessage::Hello { .. }) => {}
            SessionEvent::Lost(reason) => {
                self.fail(event_loop, anyhow!("lost the daemon: {reason}"));
            }
        }
    }
}
