//! The window: starting up, and turning window and daemon events into
//! frames.

use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use clackatoo_daemon::Sender;
use clackatoo_protocol::{Content, Event, PROTOCOL_VERSION, ServerMessage, TerminalId};
use clackatoo_term::Row;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowAttributes, WindowId};

use crate::GraphicsApi;
use crate::chrome::{self, Button, Chrome, Geometry, Hit, Status};
use crate::desk::Desk;
use crate::fonts::Fonts;
use crate::input;
use crate::layout::{Cells, Placement};
use crate::pane::Mode;
use crate::render::{CursorStyle, Highlight, PADDING, Picture, Renderer, Scene, SceneCursor};
use crate::screen::Screen;
use crate::session::{Session, SessionEvent};

/// Lines scrolled per notch of a mouse wheel.
const WHEEL_LINES: f64 = 3.0;

/// Two clicks on the same thing within this are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// A graphics instance with `backends`, for drawing on `display` if given:
/// OpenGL on Wayland draws only on the display it starts with.
fn instance(
    backends: wgpu::Backends,
    display: Option<Box<dyn wgpu::wgt::WgpuHasDisplayHandle>>,
) -> wgpu::Instance {
    let mut descriptor = match display {
        Some(display) => wgpu::InstanceDescriptor::new_with_display_handle(display),
        None => wgpu::InstanceDescriptor::new_without_display_handle(),
    };
    descriptor.backends = backends;
    // Checks on indirect draws, which we don't make. Setting them up needs
    // compute shaders, which OpenGL 3.3 (NVIDIA's on Wayland) lacks, and
    // without them the device can't be opened at all.
    descriptor
        .flags
        .remove(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL);
    wgpu::Instance::new(descriptor)
}

/// Setting up the GPU for longer than this is worth a hint at OpenGL.
const SLOW_GPU: Duration = Duration::from_millis(500);

/// The grid of panes a new window is sized for.
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
    /// Opens a GPU to draw the window with, using `graphics_api`, or the
    /// other if that can't.
    ///
    /// OpenGL on Wayland draws only on the display it started with, which
    /// arrives through `display` once the event loop has connected to it.
    /// Vulkan doesn't need it, so it starts without waiting.
    pub(crate) fn new(
        graphics_api: GraphicsApi,
        display: &mpsc::Receiver<OwnedDisplayHandle>,
    ) -> anyhow::Result<Self> {
        let mut received = None;
        let mut open = |backends: wgpu::Backends| -> anyhow::Result<Self> {
            let display = if backends.contains(wgpu::Backends::GL) {
                if received.is_none() {
                    received = Some(
                        display
                            .recv()
                            .context("the window's display never arrived")?,
                    );
                }
                received.clone()
            } else {
                None
            };
            Self::with_backends(backends, display)
        };
        if graphics_api == GraphicsApi::OpenGl {
            match open(wgpu::Backends::GL) {
                Ok(gpu) => return Ok(gpu),
                Err(err) => log::warn!("{err:#}; trying Vulkan instead of OpenGL"),
            }
        }
        // The native APIs first (Vulkan, Metal, DirectX 12). Setting up
        // OpenGL as well costs ~45 ms on Linux, for a backend only machines
        // without the others need.
        let started = Instant::now();
        let gpu = open(wgpu::Backends::PRIMARY).or_else(|err| {
            log::info!("{err:#}; trying OpenGL too");
            open(wgpu::Backends::all())
        })?;
        let took = started.elapsed();
        if took > SLOW_GPU && graphics_api == GraphicsApi::Vulkan {
            log::warn!(
                "setting up the GPU took {took:.1?}. On a machine with two GPUs, \
                 `clackatoo config renderer opengl` may open windows sooner"
            );
        }
        Ok(gpu)
    }

    fn with_backends(
        backends: wgpu::Backends,
        display: Option<OwnedDisplayHandle>,
    ) -> anyhow::Result<Self> {
        let instance = instance(
            backends,
            display.map(|display| Box::new(display) as Box<dyn wgpu::wgt::WgpuHasDisplayHandle>),
        );
        // The window doesn't exist yet, so the adapter can't be chosen by
        // the screen it draws on. Without a preference, wgpu takes the
        // first GPU the system lists, and Mesa's device-select layer lists
        // first the one that drives the screen (the boot VGA device). That
        // is what the window needs: on a desktop, the discrete GPU the
        // monitors are plugged into; on a laptop, the integrated GPU behind
        // its panel. Frames from the other GPU may never reach the
        // compositor, and a laptop's sleeping discrete GPU takes seconds to
        // wake. `WGPU_POWER_PREF` (`low` or `high`) overrides the choice. If
        // the guess is wrong, the window picks again (see `App::open`).
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or_default(),
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
        let format = surface_format(&capabilities.formats)
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
    /// The workspaces, and what's shown of them.
    desk: Desk,
    /// Where the mouse is, in pixels, while it's over the window.
    pointer: Option<[f32; 2]>,
    /// What the mouse is over.
    hover: Hit,
    /// The mouse cursor's shape, as last set.
    cursor_icon: CursorIcon,
    /// The last click, and what it was on, to spot a double click.
    last_click: Option<(Instant, Hit)>,
    /// The window's title, as last set.
    title: String,
    /// Where messages to the daemon go, once connected.
    sender: Option<Sender>,
    /// The modifier keys held now.
    modifiers: input::Modifiers,
    focused: bool,
    /// Scrolling from touchpads arrives in pixels; this is what's left
    /// over after whole lines, in lines.
    wheel_remainder: f64,
    /// Opened on the first paste.
    clipboard: Option<Clipboard>,
    /// The connection to the daemon.
    session: Session,
    /// Shown instead of the panes, while there's no connection.
    notice: Option<Notice>,
    /// Whether a frame has shown a terminal, rather than a notice.
    first_frame_drawn: bool,
    /// Why the window closed, if something went wrong.
    pub(crate) result: anyhow::Result<()>,
}

impl App {
    pub(crate) fn new(
        started: Instant,
        fonts: JoinHandle<anyhow::Result<Fonts>>,
        gpu: JoinHandle<anyhow::Result<Gpu>>,
        session: Session,
    ) -> Self {
        Self {
            started,
            startup: Some(Startup { fonts, gpu }),
            frame: None,
            desk: Desk::default(),
            pointer: None,
            hover: Hit::Nothing,
            cursor_icon: CursorIcon::Default,
            last_click: None,
            title: String::new(),
            sender: None,
            modifiers: input::Modifiers::default(),
            focused: true,
            wheel_remainder: 0.0,
            clipboard: None,
            session,
            // Shown from the first frame, so the window appears at once:
            // on Wayland, a window that hasn't drawn isn't shown at all.
            notice: Some(Notice::Connecting),
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
        let [width, height] = Geometry::window_size(
            cols,
            rows,
            [f64::from(fonts.cell.width), f64::from(fonts.cell.height)],
            PADDING,
        );
        let size = LogicalSize::new(width, height);
        let window = Arc::new(event_loop.create_window(window_attributes(size))?);
        fonts.set_scale(window.scale_factor());

        let mut gpu = startup.gpu.join().map_err(join("GPU"))??;
        let gpu_ready = self.started.elapsed();
        let mut surface = gpu.instance.create_surface(Arc::clone(&window))?;
        let config = match gpu.configure(&surface, window.inner_size()) {
            Ok(config) => config,
            Err(err) => {
                // The guessed GPU, or graphics API, can't show this window:
                // ask every API for a GPU that can, now that there's a
                // window to ask about.
                log::warn!("{err:#}; looking for another GPU");
                let instance = instance(wgpu::Backends::all(), Some(Box::new(Arc::clone(&window))));
                surface = instance.create_surface(Arc::clone(&window))?;
                let adapter =
                    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                        compatible_surface: Some(&surface),
                        ..Default::default()
                    }))
                    .context("no GPU can draw to this window")?;
                gpu = Gpu::open(instance, adapter)?;
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

    /// Tells the desk how many cells the panes have, sizing their
    /// terminals to fit.
    fn fit_panes(&mut self) {
        let area = self.geometry().map(|geometry| geometry.panes);
        self.desk.set_area(area);
    }

    /// Where things go in the window, at its size now.
    fn geometry(&self) -> Option<Geometry> {
        let frame = self.frame.as_ref()?;
        let cell = frame.fonts.cell;
        Geometry::new(
            [frame.config.width, frame.config.height],
            [cell.width, cell.height],
            padding(&frame.window),
        )
    }

    /// Where everything in the window's frame goes, now.
    fn chrome(&self) -> Option<Chrome> {
        let geometry = self.geometry()?;
        Some(if self.notice.is_some() {
            Chrome::new(
                geometry,
                Vec::new(),
                Vec::new(),
                Status::Tabs { note: None },
                false,
            )
        } else {
            Chrome::new(
                geometry,
                self.desk.workspace_names(),
                self.desk.tab_names(),
                self.desk.status(),
                true,
            )
        })
    }

    // --- The mouse --------------------------------------------------------

    /// Follows the mouse to `point`: the cursor's shape, and what's drawn
    /// lit up under it.
    fn pointer_moved(&mut self, point: [f32; 2]) {
        self.pointer = Some(point);
        let hit = self.hit(point);
        let icon = match hit {
            Hit::Resize(direction) => resize_icon(direction),
            Hit::Button(_) | Hit::Workspace(_) | Hit::Tab(_) => CursorIcon::Pointer,
            Hit::Cell { .. } => CursorIcon::Text,
            Hit::TopBar | Hit::Nothing => CursorIcon::Default,
        };
        if let Some(frame) = &self.frame
            && icon != self.cursor_icon
        {
            frame.window.set_cursor(icon);
            self.cursor_icon = icon;
        }
        let lit = |hit: Hit| matches!(hit, Hit::Button(_) | Hit::Workspace(_) | Hit::Tab(_));
        if hit != self.hover && (lit(hit) || lit(self.hover)) {
            self.request_redraw();
        }
        self.hover = hit;
    }

    fn pointer_left(&mut self) {
        self.pointer = None;
        if self.hover != Hit::Nothing {
            self.hover = Hit::Nothing;
            self.request_redraw();
        }
    }

    fn hit(&self, point: [f32; 2]) -> Hit {
        let maximised = self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.window.is_maximized());
        self.chrome()
            .map_or(Hit::Nothing, |chrome| chrome.hit(point, maximised))
    }

    /// Handles a click: moving or resizing the window, its buttons, and
    /// switching to the workspace, tab or pane clicked.
    fn clicked(&mut self, event_loop: &ActiveEventLoop, button: MouseButton) {
        let (Some(point), Some(frame)) = (self.pointer, &self.frame) else {
            return;
        };
        let window = Arc::clone(&frame.window);
        let hit = self.hit(point);
        // Twice on the same spot, soon enough, is a double click.
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(at, last)| last == hit && now.duration_since(at) < DOUBLE_CLICK);
        self.last_click = (!double).then_some((now, hit));
        if button == MouseButton::Right {
            if matches!(hit, Hit::TopBar | Hit::Workspace(_)) {
                window.show_window_menu(PhysicalPosition::new(point[0], point[1]));
            }
            return;
        }
        if button != MouseButton::Left {
            return;
        }
        let result = match hit {
            Hit::Resize(direction) => window.drag_resize_window(direction),
            Hit::Button(Button::Minimise) => {
                window.set_minimized(true);
                Ok(())
            }
            Hit::Button(Button::Maximise) => {
                window.set_maximized(!window.is_maximized());
                Ok(())
            }
            Hit::Button(Button::Close) => {
                event_loop.exit();
                Ok(())
            }
            Hit::TopBar if double => {
                window.set_maximized(!window.is_maximized());
                Ok(())
            }
            Hit::TopBar => window.drag_window(),
            Hit::Workspace(workspace) => {
                self.desk.show_workspace(workspace);
                Ok(())
            }
            Hit::Tab(tab) => {
                self.desk.show_tab(tab);
                Ok(())
            }
            Hit::Cell { col, row } => {
                self.desk.focus_cell(col, row);
                Ok(())
            }
            Hit::Nothing => Ok(()),
        };
        if let Err(err) = result {
            log::warn!("the desktop wouldn't move or resize the window: {err}");
        }
        self.update_title();
        self.request_redraw();
    }

    /// Titles the window after the focused pane, and the daemon's machine.
    fn update_title(&mut self) {
        let Some(frame) = &self.frame else { return };
        let title = self.desk.title().unwrap_or("clackatoo");
        let title = match self.session.location().host() {
            Some(host) => format!("{title} — {host}"),
            None => title.to_owned(),
        };
        if title != self.title {
            frame.window.set_title(&title);
            self.title = title;
        }
    }

    /// Sends what the desk has for the daemon.
    fn send_outbox(&mut self) {
        let messages = self.desk.take_outbox();
        let Some(sender) = &mut self.sender else {
            return;
        };
        for message in messages {
            if let Err(err) = sender.send(&message) {
                log::warn!("sending to the daemon failed: {err}");
            }
        }
    }

    /// Handles a key press: a notice's keys, then the desk's.
    fn key_pressed(&mut self, event_loop: &ActiveEventLoop, event: &winit::event::KeyEvent) {
        if self.notice.is_some() {
            self.notice_key(event_loop, event);
            return;
        }
        let redraw = if is_paste(&event.logical_key, self.modifiers) {
            match paste_text(&mut self.clipboard) {
                Ok(text) => self.desk.paste(&text),
                Err(err) => log::warn!("pasting failed: {err:#}"),
            }
            true
        } else {
            self.desk.key(&input::KeyPress {
                key: &event.logical_key,
                text: event.text.as_deref(),
                location: event.location,
                code: match event.physical_key {
                    winit::keyboard::PhysicalKey::Code(code) => Some(code),
                    winit::keyboard::PhysicalKey::Unidentified(_) => None,
                },
                modifiers: self.modifiers,
            })
        };
        self.update_title();
        if redraw {
            self.request_redraw();
        }
    }

    /// Handles a key while a notice is shown: answering its question, or
    /// typing the answer to ssh's.
    fn notice_key(&mut self, event_loop: &ActiveEventLoop, event: &winit::event::KeyEvent) {
        let Some(notice) = &mut self.notice else {
            return;
        };
        let key = &event.logical_key;
        let enter = matches!(key, Key::Named(NamedKey::Enter));
        let escape = matches!(key, Key::Named(NamedKey::Escape));
        match notice {
            Notice::Ask { input, answer, .. } => {
                if enter || escape {
                    let reply = enter.then(|| std::mem::take(&mut input.0));
                    // ssh has given up if this fails, and the session
                    // thread will say why.
                    let _ = answer.send(reply);
                    self.notice = Some(Notice::Connecting);
                } else if is_paste(key, self.modifiers) {
                    match paste_text(&mut self.clipboard) {
                        Ok(text) => input.push(&text),
                        Err(err) => log::warn!("pasting failed: {err:#}"),
                    }
                } else if matches!(key, Key::Named(NamedKey::Backspace)) {
                    input.0.pop();
                } else if self.modifiers.ctrl {
                    if matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("u")) {
                        input.0.clear();
                    }
                } else if let Some(text) = &event.text {
                    input.push(text);
                }
            }
            Notice::OtherDaemon { .. } if enter => {
                self.notice = Some(Notice::Replacing);
                self.session.replace_daemon();
            }
            Notice::Lost { .. } if enter => {
                self.notice = Some(Notice::Connecting);
                self.session.reconnect();
            }
            Notice::OtherDaemon { .. } | Notice::Lost { .. } | Notice::Connecting if escape => {
                event_loop.exit();
            }
            _ => return,
        }
        self.request_redraw();
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        if self.notice.is_some() {
            return;
        }
        let Some(cell_height) = self.frame.as_ref().map(|frame| frame.fonts.cell.height) else {
            return;
        };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * WHEEL_LINES,
            MouseScrollDelta::PixelDelta(position) => position.y / f64::from(cell_height),
        } + self.wheel_remainder;
        let whole = lines.trunc();
        self.wheel_remainder = lines - whole;
        #[allow(clippy::cast_possible_truncation)]
        if whole != 0.0 && self.desk.wheel(whole as i64) {
            self.request_redraw();
        }
    }

    fn request_redraw(&self) {
        if let Some(frame) = &self.frame {
            frame.window.request_redraw();
        }
    }

    // --- Drawing ----------------------------------------------------------

    fn draw(&mut self) {
        let started = Instant::now();
        let Some(chrome) = self.chrome() else {
            return;
        };
        if let Some(notice) = &self.notice {
            let geometry = chrome.geometry;
            let lines = notice.lines(
                self.session.location().host(),
                usize::from(geometry.grid.cols()),
            );
            let screen = Screen::message(&lines, geometry.grid);
            let rows: Vec<_> = screen.rows.iter().collect();
            let scenes = [Scene {
                origin: geometry.grid_origin(),
                size: geometry.grid_pixels(),
                rows: &rows,
                cursor: None,
                normal_mode: false,
                highlights: &[],
            }];
            let (fills, texts) = chrome.paint(self.hover);
            if let Some(frame) = &mut self.frame {
                present(
                    frame,
                    &Picture {
                        scenes: &scenes,
                        fills: &fills,
                        texts: &texts,
                    },
                );
            }
            return;
        }
        let Some(drawn) = self.draw_panes(&chrome) else {
            return;
        };
        // For measuring: `RUST_LOG=clackatoo_gui::frame=trace`.
        log::trace!(target: "clackatoo_gui::frame", "frame {}", started.elapsed().as_micros());
        // What the benchmarks time: a terminal on screen, not the notice
        // shown while connecting.
        if !drawn.is_empty() && !self.first_frame_drawn {
            self.first_frame_drawn = true;
            log::info!("first frame after {:.1?}", self.started.elapsed());
        }
        self.desk.drawn(&drawn);
    }

    /// Draws the frame and the shown tab's panes. Returns the terminals
    /// drawn, or `None` if the frame wasn't shown.
    fn draw_panes(&mut self, chrome: &Chrome) -> Option<Vec<TerminalId>> {
        let geometry = &chrome.geometry;
        let desk = &self.desk;
        let shown = desk.shown()?;
        let tab = desk.shown_tab()?;
        let placement = Placement::new(&tab.layout, geometry.panes);
        let layout_panes = tab.layout.panes();
        let mut drawings = Vec::new();
        for &(id, cells) in &placement.panes {
            let Some(pane) = layout_panes.iter().find(|pane| pane.id == id) else {
                continue;
            };
            let Content::Terminal(terminal) = pane.content;
            let Some(state) = desk.pane(terminal) else {
                continue;
            };
            if state.screen().is_none() {
                continue;
            }
            let height = usize::from(cells.rows);
            let (rows, mut cursor) = state.scene(height);
            let focused = id == shown.pane;
            if let Some(cursor) = &mut cursor
                && !(focused && self.focused)
            {
                cursor.style = CursorStyle::Hollow;
            }
            drawings.push(Drawing {
                terminal,
                cells,
                rows,
                cursor,
                highlights: state.highlights(height),
                // Only the focused pane's mode matters, as only it gets keys.
                normal_mode: focused && state.mode != Mode::Insert,
            });
        }
        let scenes: Vec<Scene<'_>> = drawings
            .iter()
            .map(|drawing| Scene {
                origin: geometry.origin(drawing.cells),
                size: geometry.pixels(drawing.cells),
                rows: &drawing.rows,
                cursor: drawing.cursor,
                normal_mode: drawing.normal_mode,
                highlights: &drawing.highlights,
            })
            .collect();
        let (mut fills, texts) = chrome.paint(self.hover);
        fills.extend(
            placement
                .dividers
                .iter()
                .map(|divider| chrome::divider_fill(geometry, divider)),
        );
        let picture = Picture {
            scenes: &scenes,
            fills: &fills,
            texts: &texts,
        };
        let frame = self.frame.as_mut()?;
        if !present(frame, &picture) {
            return None;
        }
        Some(drawings.iter().map(|drawing| drawing.terminal).collect())
    }
}

/// The cursor for resizing the window towards `direction`.
fn resize_icon(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::NResize,
        ResizeDirection::South => CursorIcon::SResize,
        ResizeDirection::East => CursorIcon::EResize,
        ResizeDirection::West => CursorIcon::WResize,
        ResizeDirection::NorthEast => CursorIcon::NeResize,
        ResizeDirection::NorthWest => CursorIcon::NwResize,
        ResizeDirection::SouthEast => CursorIcon::SeResize,
        ResizeDirection::SouthWest => CursorIcon::SwResize,
    }
}

/// One pane, ready to draw.
struct Drawing<'a> {
    terminal: TerminalId,
    cells: Cells,
    rows: Vec<&'a Row>,
    cursor: Option<SceneCursor>,
    highlights: Vec<Highlight>,
    normal_mode: bool,
}

/// The format to draw the window in, from those it supports. Colours are
/// blended in sRGB, as other terminals do, so text has its usual weight:
/// that needs a format that stores the shader's values as they are, so not
/// an `…Srgb` one, which converts them. And not a floating-point one
/// either, which some GPUs (Intel's) list first: the compositor takes its
/// values as linear light, so the colours come out washed out.
fn surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    use wgpu::TextureFormat::{Bgra8Unorm, Rgb10a2Unorm, Rgba8Unorm};
    [Bgra8Unorm, Rgba8Unorm, Rgb10a2Unorm]
        .into_iter()
        .find(|format| formats.contains(format))
        .or_else(|| formats.iter().copied().find(|format| !format.is_srgb()))
        .or_else(|| formats.first().copied())
}

/// Draws `picture` into the window and shows it. Returns whether a frame
/// was shown.
fn present(frame: &mut Frame, picture: &Picture<'_>) -> bool {
    let texture = match frame.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(texture)
        | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
        wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
            return false;
        }
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            frame.surface.configure(&frame.gpu.device, &frame.config);
            frame.window.request_redraw();
            return false;
        }
        wgpu::CurrentSurfaceTexture::Validation => {
            log::error!("the GPU refused a frame");
            return false;
        }
    };
    let view = texture
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    frame.renderer.render(
        (&frame.gpu.device, &frame.gpu.queue),
        &view,
        [frame.config.width, frame.config.height],
        picture,
        &mut frame.fonts,
    );
    frame.window.pre_present_notify();
    frame.gpu.queue.present(texture);
    true
}

/// Whether a key pastes: Ctrl+Shift+V.
fn is_paste(key: &Key, modifiers: input::Modifiers) -> bool {
    modifiers.ctrl
        && modifiers.shift
        && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("v"))
}

/// Whether ssh's `question` wants a secret, such as a password, rather
/// than an answer that can be shown, such as yes or no to trusting a new
/// host.
fn is_secret(question: &str) -> bool {
    !question.to_lowercase().contains("yes/no")
}

/// Text typed into a notice. It may be a password, so it never appears in
/// debug output.
#[derive(Default)]
struct Typed(String);

impl Typed {
    /// Adds `text`, without control characters such as newlines.
    fn push(&mut self, text: &str) {
        self.0.extend(text.chars().filter(|ch| !ch.is_control()));
    }
}

impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Typed(..)")
    }
}

/// A message the window shows instead of the terminal.
#[derive(Debug)]
enum Notice {
    /// A daemon from another build is running; replace it?
    OtherDaemon { protocol: u32 },
    /// Replacing it.
    Replacing,
    /// The connection was lost, or never made; try again?
    Lost { reason: String },
    /// Connecting, or trying again.
    Connecting,
    /// ssh asks a question, such as for a password, while connecting.
    Ask {
        question: String,
        /// The answer so far.
        input: Typed,
        /// Whether to hide the answer.
        secret: bool,
        answer: mpsc::Sender<Option<String>>,
    },
}

impl Notice {
    /// The notice's text, for the daemon on `host`, or this machine's, in
    /// lines of at most `width` characters.
    fn lines(&self, host: Option<&str>, width: usize) -> Vec<String> {
        let on_host = host.map(|host| format!(" on {host}")).unwrap_or_default();
        let paragraphs = match self {
            Self::OtherDaemon { protocol } => vec![
                format!(
                    "The clackatoo daemon{on_host} is from another build. It speaks \
                     protocol {protocol}, and this window speaks {PROTOCOL_VERSION}."
                ),
                "Replacing it closes every terminal running in it, and hangs up their \
                 programs."
                    .into(),
                "  Enter  replace it\n  Esc    quit".into(),
            ],
            Self::Replacing => vec![format!("Replacing the daemon{on_host}…")],
            Self::Lost { reason } => vec![
                format!("Lost the connection to the daemon{on_host}."),
                reason.clone(),
                "  Enter  reconnect\n  Esc    quit".into(),
            ],
            Self::Connecting => vec![format!("Connecting to the daemon{on_host}…")],
            Self::Ask {
                question,
                input,
                secret,
                ..
            } => {
                let shown = if *secret {
                    "•".repeat(input.0.chars().count())
                } else {
                    input.0.clone()
                };
                vec![
                    format!("Logging in to {}.", host.unwrap_or("the daemon's machine")),
                    question.trim().to_owned(),
                    format!("> {shown}▏"),
                    "  Enter  answer\n  Esc    cancel".into(),
                ]
            }
        };
        let mut lines = vec![String::new()];
        for paragraph in paragraphs {
            for line in paragraph.lines() {
                lines.extend(wrap(line, width.saturating_sub(4).max(20), "  "));
            }
            lines.push(String::new());
        }
        lines
    }
}

/// `text` in lines of at most `width` characters, each after `indent`. Text
/// that fits is kept as it is; text that doesn't is broken between words.
fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    if text.chars().count() <= width {
        return vec![format!("{indent}{text}")];
    }
    let mut lines = Vec::new();
    let mut line = indent.to_owned();
    let mut empty = true;
    for word in text.split_whitespace() {
        let length = line.chars().count() - indent.len();
        if !empty && length + 1 + word.chars().count() > width {
            lines.push(std::mem::replace(&mut line, indent.to_owned()));
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    lines.push(line);
    lines
}

/// The new window's title, size and app ID. Desktops find the window's
/// icon through the app ID, matching it to `clackatoo.desktop`. The window
/// draws its own frame (see [`chrome`]), so it asks for none.
fn window_attributes(size: LogicalSize<f64>) -> WindowAttributes {
    let attributes = Window::default_attributes()
        .with_title("clackatoo")
        .with_inner_size(size)
        .with_decorations(false);
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
                // Whatever arrived while the window opened is drawn now:
                // the notice, or panes whose snapshots came first.
                frame.window.request_redraw();
                self.frame = Some(frame);
                self.fit_panes();
            }
            Err(err) => self.fail(event_loop, err),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        self.handle_window_event(event_loop, event);
        self.send_outbox();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: SessionEvent) {
        self.handle_session_event(event);
        self.send_outbox();
        if self.desk.finished() {
            event_loop.exit();
        }
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
                self.fit_panes();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(frame) = &mut self.frame {
                    frame.fonts.set_scale(scale_factor);
                    frame.renderer.forget_glyphs();
                    frame.window.request_redraw();
                }
                self.fit_panes();
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
            } if event.state.is_pressed() => self.key_pressed(event_loop, &event),
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            #[allow(clippy::cast_possible_truncation)]
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer_moved([position.x as f32, position.y as f32]);
            }
            WindowEvent::CursorLeft { .. } => self.pointer_left(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => self.clicked(event_loop, button),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                // Another window on the terminals may have resized them:
                // the focused one decides.
                if focused {
                    self.fit_panes();
                }
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Connected { sender, workspaces } => {
                self.notice = None;
                self.sender = Some(sender);
                self.desk.connected(workspaces);
                self.update_title();
                self.request_redraw();
            }
            SessionEvent::OtherDaemon { protocol } => {
                self.notice = Some(Notice::OtherDaemon { protocol });
                self.request_redraw();
            }
            SessionEvent::Ask { question, answer } => {
                self.notice = Some(Notice::Ask {
                    secret: is_secret(&question),
                    question,
                    input: Typed::default(),
                    answer,
                });
                self.request_redraw();
            }
            SessionEvent::Message(ServerMessage::Event(Event::View { terminal, update })) => {
                if self.desk.view_changed(terminal, &update) {
                    self.request_redraw();
                }
                if self.desk.focused_terminal() == Some(terminal) {
                    self.update_title();
                }
            }
            SessionEvent::Message(ServerMessage::Event(Event::Workspaces(workspaces))) => {
                self.desk.workspaces_changed(workspaces);
                self.update_title();
                self.request_redraw();
            }
            SessionEvent::Message(ServerMessage::Response(response)) => {
                let redraw = self.desk.answered(response);
                if let Some(text) = self.desk.take_yanked()
                    && let Err(err) = copy_text(&mut self.clipboard, text)
                {
                    log::warn!("copying failed: {err:#}");
                }
                self.update_title();
                if redraw {
                    self.request_redraw();
                }
            }
            SessionEvent::Message(ServerMessage::Hello { .. }) => {}
            SessionEvent::Lost(reason) => {
                log::warn!("lost the daemon: {reason}");
                self.sender = None;
                self.desk.lost();
                self.notice = Some(Notice::Lost { reason });
                self.request_redraw();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_wrap_between_words_and_keep_lines_that_fit() {
        assert_eq!(
            wrap("one two three four", 9, "  "),
            ["  one two", "  three", "  four"]
        );
        assert_eq!(wrap("  Enter  go", 20, "  "), ["    Enter  go"]);
        assert_eq!(wrap("", 9, "  "), ["  "]);
        // A word longer than a line gets a line of its own.
        assert_eq!(wrap("a abcdefghijk b", 5, ""), ["a", "abcdefghijk", "b"]);
    }

    #[test]
    fn the_window_is_drawn_in_an_eight_bit_format_that_doesnt_convert() {
        use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba16Float};
        // As Intel's Vulkan driver lists them.
        assert_eq!(
            surface_format(&[Rgba16Float, Bgra8UnormSrgb, Bgra8Unorm]),
            Some(Bgra8Unorm)
        );
        assert_eq!(surface_format(&[Rgba8Unorm]), Some(Rgba8Unorm));
        assert_eq!(surface_format(&[Rgba16Float]), Some(Rgba16Float));
        assert_eq!(surface_format(&[]), None);
    }

    #[test]
    fn passwords_are_hidden_but_host_confirmations_are_not() {
        assert!(is_secret("jack@desktop's password: "));
        assert!(is_secret(
            "Enter passphrase for key '/home/jack/.ssh/id_ed25519': "
        ));
        assert!(!is_secret(
            "The authenticity of host 'desktop' can't be established.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        ));
    }

    #[test]
    fn typed_answers_never_show_in_debug_output() {
        let mut typed = Typed::default();
        typed.push("hunter2\n");
        assert_eq!(typed.0, "hunter2", "control characters are dropped");
        assert_eq!(format!("{typed:?}"), "Typed(..)");
    }
}
