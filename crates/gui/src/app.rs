//! The window: starting up, and turning window and daemon events into
//! frames.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, anyhow};
use clackatoo_daemon::Sender;
use clackatoo_protocol::{
    ClientMessage, Event, Request, Response, ServerMessage, Size, TerminalId,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::fonts::Fonts;
use crate::render::{PADDING, Renderer};
use crate::screen::Screen;
use crate::session::SessionEvent;

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
        // to hand frames to the compositor at all.
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no GPU found")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("clackatoo"),
            ..Default::default()
        }))
        .context("could not open the GPU")?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
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

/// The terminal the window shows.
#[derive(Debug)]
struct Attached {
    sender: Sender,
    terminal: TerminalId,
    /// The size last asked of the daemon.
    requested: Option<Size>,
}

/// The window's whole state.
#[derive(Debug)]
pub(crate) struct App {
    started: Instant,
    startup: Option<Startup>,
    frame: Option<Frame>,
    attached: Option<Attached>,
    screen: Option<Screen>,
    /// A view update has been applied but not yet drawn and acknowledged.
    unacknowledged: bool,
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
            attached: None,
            screen: None,
            unacknowledged: false,
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
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("clackatoo")
                    .with_inner_size(size),
            )?,
        );
        fonts.set_scale(window.scale_factor());

        let gpu = startup.gpu.join().map_err(join("GPU"))??;
        let gpu_ready = self.started.elapsed();
        let surface = gpu.instance.create_surface(Arc::clone(&window))?;
        if !gpu.adapter.is_surface_supported(&surface) {
            anyhow::bail!("the GPU can't draw to this window");
        }
        let capabilities = surface.get_capabilities(&gpu.adapter);
        // Colours are blended in sRGB, as other terminals do, so text has its
        // usual weight. That needs a format that doesn't convert.
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
        let PhysicalSize { width, height } = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            desired_maximum_frame_latency: 2,
            present_mode,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);
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
        let (Some(frame), Some(attached)) = (&self.frame, &mut self.attached) else {
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
        if attached.requested == Some(size) {
            return;
        }
        attached.requested = Some(size);
        let request = ClientMessage::Request(Request::Resize {
            terminal: attached.terminal,
            size,
        });
        if let Err(err) = attached.sender.send(&request) {
            log::warn!("asking for a resize failed: {err}");
        }
    }

    fn draw(&mut self) {
        let (Some(frame), Some(screen)) = (&mut self.frame, &self.screen) else {
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
        frame.renderer.render(
            (&frame.gpu.device, &frame.gpu.queue),
            &view,
            [frame.config.width, frame.config.height],
            padding,
            screen,
            &mut frame.fonts,
        );
        frame.window.pre_present_notify();
        frame.gpu.queue.present(texture);

        if !self.first_frame_drawn {
            self.first_frame_drawn = true;
            log::info!("first frame after {:.1?}", self.started.elapsed());
        }
        if std::mem::take(&mut self.unacknowledged)
            && let Some(attached) = &mut self.attached
        {
            let done = ClientMessage::FrameDone {
                terminal: attached.terminal,
            };
            if let Err(err) = attached.sender.send(&done) {
                log::warn!("acknowledging a frame failed: {err}");
            }
        }
    }
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
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: SessionEvent) {
        match event {
            SessionEvent::Attached { sender, terminal } => {
                self.attached = Some(Attached {
                    sender,
                    terminal,
                    requested: None,
                });
                self.fit_terminal();
            }
            SessionEvent::Message(ServerMessage::Event(Event::View { terminal, update })) => {
                if self
                    .attached
                    .as_ref()
                    .is_none_or(|attached| attached.terminal != terminal)
                {
                    return;
                }
                match &mut self.screen {
                    Some(screen) => screen.apply(&update),
                    None => match &update {
                        clackatoo_protocol::ViewUpdate::Snapshot(view) => {
                            self.screen = Some(Screen::new(view));
                        }
                        clackatoo_protocol::ViewUpdate::Patch(_) => return,
                    },
                }
                self.unacknowledged = true;
                if let (Some(frame), Some(screen)) = (&self.frame, &self.screen) {
                    let title = if screen.title.is_empty() {
                        "clackatoo"
                    } else {
                        &screen.title
                    };
                    frame.window.set_title(title);
                    frame.window.request_redraw();
                }
            }
            SessionEvent::Message(ServerMessage::Response(Response::Error(err))) => {
                log::warn!("the daemon refused a request: {err}");
            }
            SessionEvent::Message(_) => {}
            SessionEvent::Lost(reason) => {
                self.fail(event_loop, anyhow!("lost the daemon: {reason}"));
            }
        }
    }
}
