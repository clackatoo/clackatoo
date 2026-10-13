//! The window: starting up, and turning window and daemon events into
//! frames.

use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use arboard::{GetExtLinux, LinuxClipboardKind, SetExtLinux};
use clackatoo_daemon::launch;
use clackatoo_daemon::{Contact, Sender};
use clackatoo_protocol::{
    Axis, Content, Event, Layout, PROTOCOL_VERSION, Pane as LayoutPane, PaneId, ServerMessage,
    Size, TabId, TerminalId, WorkspaceId,
};
use clackatoo_term::Row;
use clackatoo_term::mouse::MouseAction;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, OwnedDisplayHandle};
use winit::keyboard::{Key, KeyLocation, NamedKey};
use winit::window::{CursorIcon, ImePurpose, ResizeDirection, Window, WindowAttributes, WindowId};

use crate::GraphicsApi;
use crate::chat::{ChatPaint, Pointer};
use crate::chrome::{self, Chrome, Contents, Geometry, Hit, Metrics, Status};
use crate::desk::{Desk, TextTarget};
use crate::desktop::{Button, ButtonLayout, Desktop};
use crate::fonts::{CellMetrics, Fonts};
use crate::ime::{InputMethod, Preedit};
use crate::input;
use crate::layout::{Placement, Tile};
use crate::pane::{Copy, Mode};
use crate::render::{
    self, CursorStyle, Fill, Highlight, PADDING, Paint, Picture, Renderer, Scene, SceneCursor,
};
use crate::screen::Screen;
use crate::session::{Session, SessionEvent};
use crate::updates;

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
const INITIAL_GRID: (u16, u16) = (100, 30);

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
        // See-through where nothing's drawn, so the window can round its
        // corners, if the compositor allows.
        let alpha_mode = if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            wgpu::CompositeAlphaMode::Auto
        };
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
            alpha_mode,
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
    fonts: JoinHandle<anyhow::Result<(Fonts, Desktop)>>,
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
    /// Whether the compositor shows what's behind the window where it's
    /// see-through, which lets the window round its corners.
    transparent: bool,
}

/// A menu open over the window, for a workspace or a tab.
#[derive(Clone, Copy, Debug)]
struct Menu {
    /// Where it was asked for.
    at: [f32; 2],
    target: MenuTarget,
}

/// What a menu is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuTarget {
    Workspace(WorkspaceId),
    Tab(TabId),
}

/// A menu's items, in order.
const MENU_ITEMS: [&str; 2] = ["Rename", "Close"];

/// The window's whole state.
#[derive(Debug)]
pub(crate) struct App {
    started: Instant,
    startup: Option<Startup>,
    frame: Option<Frame>,
    /// The workspaces, and what's shown of them.
    desk: Desk,
    /// Where the window's buttons go, as the desktop says.
    buttons: ButtonLayout,
    /// Where the mouse is, in pixels, while it's over the window.
    pointer: Option<[f32; 2]>,
    /// What the mouse is over.
    hover: Hit,
    /// The mouse cursor's shape, as last set.
    cursor_icon: CursorIcon,
    /// The last click, what it was on and where, and how many came quickly
    /// one after another there, to spot double and triple clicks.
    last_click: Option<(Instant, Hit, [f32; 2], u8)>,
    /// The menu open, if any.
    menu: Option<Menu>,
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
    /// Whether a thread is reading the clipboard, for the editors.
    reading_clipboard: Arc<std::sync::atomic::AtomicBool>,
    /// The connection to the daemon.
    session: Session,
    /// Shown instead of the panes, while there's no connection.
    notice: Option<Notice>,
    /// Whether a daemon on another machine is in contact.
    contact: Option<Contact>,
    /// Whether a frame has shown a terminal, rather than a notice.
    first_frame_drawn: bool,
    /// The input method: whether it's on, which it is while keys type
    /// text, and what it's composing.
    ime: InputMethod,
    /// Where the input method was last told the cursor is, in pixels.
    ime_area: Option<[f32; 4]>,
    /// Whether a chat shows what the input method is composing, in its
    /// box, so the window doesn't.
    composing_in_chat: bool,
    /// New releases, and how the daemon's differs from the window's.
    updates: updates::Known,
    /// Why the window closed, if something went wrong.
    pub(crate) result: anyhow::Result<()>,
}

impl App {
    pub(crate) fn new(
        started: Instant,
        fonts: JoinHandle<anyhow::Result<(Fonts, Desktop)>>,
        gpu: JoinHandle<anyhow::Result<Gpu>>,
        session: Session,
    ) -> Self {
        Self {
            started,
            startup: Some(Startup { fonts, gpu }),
            frame: None,
            desk: Desk::default(),
            buttons: ButtonLayout::default(),
            pointer: None,
            hover: Hit::Nothing,
            cursor_icon: CursorIcon::Default,
            last_click: None,
            menu: None,
            title: String::new(),
            sender: None,
            modifiers: input::Modifiers::default(),
            focused: true,
            wheel_remainder: 0.0,
            clipboard: None,
            reading_clipboard: Arc::default(),
            session,
            // Shown from the first frame, so the window appears at once:
            // on Wayland, a window that hasn't drawn isn't shown at all.
            notice: Some(Notice::Connecting),
            contact: None,
            first_frame_drawn: false,
            ime: InputMethod::default(),
            ime_area: None,
            composing_in_chat: false,
            updates: updates::Known::new(),
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
        let (mut fonts, desktop) = startup.fonts.join().map_err(join("font"))??;
        let fonts_ready = self.started.elapsed();
        self.buttons = desktop.buttons;
        // Fonts were loaded at one pixel per point; the window's display
        // may have more.
        let (cols, rows) = INITIAL_GRID;
        #[allow(clippy::cast_possible_truncation)]
        let [width, height] = Geometry::window_size(cols, rows, metrics(&fonts, PADDING as f32));
        let size = LogicalSize::new(f64::from(width), f64::from(height));
        let window = Arc::new(event_loop.create_window(window_attributes(size))?);
        window.set_ime_purpose(ImePurpose::Terminal);
        log::debug!(
            "asked for a {size:?} window; got {:?} at scale {}",
            window.inner_size(),
            window.scale_factor()
        );
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
        let transparent = config.alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied;
        Ok(Frame {
            window,
            surface,
            config,
            gpu,
            renderer,
            fonts,
            transparent,
        })
    }

    /// Tells the desk how many cells the panes have, sizing their
    /// terminals to fit.
    fn fit_panes(&mut self) {
        let area = self.geometry().map(|geometry| geometry.area);
        self.desk.set_area(area);
    }

    /// Where things go in the window, at its size now.
    fn geometry(&self) -> Option<Geometry> {
        let frame = self.frame.as_ref()?;
        #[allow(clippy::cast_precision_loss)]
        let padding = padding(&frame.window) as f32;
        Some(Geometry::new(
            [frame.config.width, frame.config.height],
            metrics(&frame.fonts, padding),
        ))
    }

    /// Where everything in the window's frame goes, now.
    fn chrome(&mut self) -> Option<Chrome> {
        let geometry = self.geometry()?;
        let rounded = self.rounded();
        let contents = if self.notice.is_some() {
            Contents {
                workspaces: Vec::new(),
                tabs: Vec::new(),
                status: Status::Tabs {
                    mode: None,
                    note: None,
                    panes: 0,
                },
                buttons: &self.buttons,
                bottom_shown: false,
                menu: None,
                alert: None,
                rounded,
                finder: None,
            }
        } else {
            let mut status = self.desk.status();
            // What the input method is composing shows at the caret, where
            // it'll go, until it's committed.
            if let Status::Typing { text, .. } = &mut status
                && let Some(preedit) = self.ime.preedit()
                && self.desk.text_goes_to() == Some(TextTarget::Bar)
                && let Some(caret) = text.rfind(CARET)
            {
                text.insert_str(caret, &preedit.text);
            }
            let finder = crate::finder::view::Layout::new(&geometry);
            let layout = (finder.rows, finder.cols);
            let mut finder = self.desk.finder_view(layout, finder.preview_size);
            // And so in the finder's query.
            if let Some(view) = &mut finder
                && let Some(preedit) = self.ime.preedit()
                && let Some(caret) = view.query.rfind(CARET)
            {
                view.query.insert_str(caret, &preedit.text);
            }
            Contents {
                workspaces: self.desk.workspace_names(),
                tabs: self.desk.tab_names(),
                status,
                buttons: &self.buttons,
                bottom_shown: true,
                menu: self
                    .menu
                    .map(|menu| (menu.at, MENU_ITEMS.map(str::to_owned).to_vec())),
                alert: self.out_of_contact(),
                rounded,
                finder,
            }
        };
        let fonts = &mut self.frame.as_mut()?.fonts;
        Some(Chrome::new(geometry, contents, &mut |text, bold| {
            fonts.ui_width(text, bold)
        }))
    }

    /// What the top bar says while a daemon on another machine is out of
    /// contact: since when, to the second.
    fn out_of_contact(&self) -> Option<String> {
        let since = self.contact.as_ref()?.silent_since()?;
        let host = self.session.location().host().unwrap_or("the daemon");
        Some(format!(
            "No contact with {host} for {} s",
            since.elapsed().as_secs()
        ))
    }

    // --- The mouse --------------------------------------------------------

    /// Follows the mouse to `point`: the cursor's shape, and what's drawn
    /// lit up under it.
    fn pointer_moved(&mut self, point: [f32; 2]) {
        self.pointer = Some(point);
        let hit = self.hit(point);
        let icon = match hit {
            Hit::Resize(direction) => resize_icon(direction),
            Hit::Button(_)
            | Hit::Workspace(_)
            | Hit::Tab(_)
            | Hit::NewWorkspace
            | Hit::NewTab
            | Hit::Menu(_)
            | Hit::Finder(Some(_)) => CursorIcon::Pointer,
            Hit::Panes if let Some(axis) = self.desk.divider_at(point) => match axis {
                Axis::LeftToRight => CursorIcon::ColResize,
                Axis::TopToBottom => CursorIcon::RowResize,
            },
            // A link a click would open, with Ctrl held.
            Hit::Panes if self.desk.link_at(point, self.modifiers) => CursorIcon::Pointer,
            // A program that takes the mouse gets the arrow, as clicks are
            // its, not for selecting text.
            Hit::Panes if self.desk.reports_mouse_at(point, self.modifiers.shift) => {
                CursorIcon::Default
            }
            // A chat says what's under the pointer.
            Hit::Panes if let Some(pointer) = self.desk.chat_pointer(point) => match pointer {
                Pointer::Arrow => CursorIcon::Default,
                Pointer::Hand => CursorIcon::Pointer,
                Pointer::Text => CursorIcon::Text,
            },
            Hit::Panes => CursorIcon::Text,
            Hit::TopBar | Hit::Finder(None) | Hit::Nothing => CursorIcon::Default,
        };
        if let Some(frame) = &self.frame
            && icon != self.cursor_icon
        {
            frame.window.set_cursor(icon);
            self.cursor_icon = icon;
        }
        let lit = |hit: Hit| {
            matches!(
                hit,
                Hit::Button(_)
                    | Hit::Workspace(_)
                    | Hit::Tab(_)
                    | Hit::NewWorkspace
                    | Hit::NewTab
                    | Hit::Menu(_)
            )
        };
        if hit != self.hover && (lit(hit) || lit(self.hover)) {
            self.request_redraw();
        }
        self.hover = hit;
        if self.desk.drag_divider(point) {
            self.send_outbox();
            self.request_redraw();
            return;
        }
        // Over a pane, or dragging out of one: not over the finder.
        if self.notice.is_none() && !matches!(hit, Hit::Finder(_)) {
            let outcome = self
                .desk
                .mouse(point, MouseAction::Motion(None), 1, self.modifiers);
            self.send_outbox();
            if outcome.redraw {
                self.request_redraw();
            }
        }
    }

    /// A mouse button went down in a pane, the `clicks`th time in a row.
    fn clicked_in_pane(&mut self, point: [f32; 2], button: MouseButton, clicks: u8) {
        // Between panes, the left button grabs the divider.
        if button == MouseButton::Left && self.desk.grab_divider(point, clicks) {
            self.send_outbox();
            self.request_redraw();
            return;
        }
        let Some(button) = mouse_button(button) else {
            return;
        };
        let outcome = self
            .desk
            .mouse(point, MouseAction::Press(button), clicks, self.modifiers);
        if outcome.paste_primary {
            match paste_text(&mut self.clipboard, Copy::Primary) {
                Ok(text) => self.desk.paste(&text),
                Err(err) => log::warn!("pasting the primary selection failed: {err:#}"),
            }
        }
        self.open_links();
        self.update_title();
        self.send_outbox();
        self.request_redraw();
    }

    /// A mouse button came up: the end of a click or drag in a pane.
    fn released(&mut self, button: MouseButton) {
        if button == MouseButton::Left && self.desk.drop_divider() {
            return;
        }
        let (Some(point), Some(button)) = (self.pointer, mouse_button(button)) else {
            return;
        };
        if self.notice.is_some() || matches!(self.hit(point), Hit::Finder(_)) {
            return;
        }
        let outcome = self
            .desk
            .mouse(point, MouseAction::Release(button), 1, self.modifiers);
        self.send_outbox();
        if outcome.redraw {
            self.request_redraw();
        }
    }

    fn pointer_left(&mut self) {
        self.pointer = None;
        if self.desk.mouse_left() {
            self.request_redraw();
        }
        if self.hover != Hit::Nothing {
            self.hover = Hit::Nothing;
            self.request_redraw();
        }
    }

    /// Hands the URLs links asked to open to the desktop's opener.
    fn open_links(&mut self) {
        for url in self.desk.take_opened() {
            crate::desktop::open(&url);
        }
    }

    /// Carries out `:update`, if it was typed: runs `clackatoo update`
    /// here, or opens a tab running it on the daemon's machine, where it
    /// can be watched.
    fn update(&mut self) {
        use crate::desk::UpdateAsked;
        let Some(asked) = self.desk.take_update_asked() else {
            return;
        };
        match (asked, self.session.location().remote()) {
            (UpdateAsked::Here, _) => match &self.updates.program {
                Some(program) => updates::update(program.clone(), self.session.proxy()),
                None => self
                    .desk
                    .say("this program can't be found, to update it".into()),
            },
            (UpdateAsked::On(host), Some(remote)) if host == remote.host => {
                let command = format!("{} update", remote.command);
                self.desk
                    .open_tab_running(vec!["sh".into(), "-c".into(), command]);
            }
            (UpdateAsked::On(host), Some(remote)) => self.desk.say(format!(
                "this window's daemon is on {}, not {host}",
                remote.host
            )),
            (UpdateAsked::On(_), None) => self
                .desk
                .say("this window's daemon is on this machine, which :update alone updates".into()),
        }
        self.send_outbox();
    }

    /// Puts what the window knows of releases in the bar, and says `news`
    /// until the next key.
    fn tell_updates(&mut self, news: Option<String>) {
        self.desk.set_notice(self.updates.notice());
        if let Some(news) = news {
            self.desk.say(news);
        }
        self.request_redraw();
    }

    fn hit(&mut self, point: [f32; 2]) -> Hit {
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
        #[allow(clippy::cast_precision_loss)]
        let cell = [
            frame.fonts.cell.width as f32,
            frame.fonts.cell.height as f32,
        ];
        let hit = self.hit(point);
        // Twice on the same thing, soon enough and within a cell of each
        // other, is a double click, and three times a triple.
        let now = Instant::now();
        let near =
            |[x, y]: [f32; 2]| (x - point[0]).abs() < cell[0] && (y - point[1]).abs() < cell[1];
        let clicks = match self.last_click {
            Some((at, last, where_, clicks))
                if last == hit && near(where_) && now.duration_since(at) < DOUBLE_CLICK =>
            {
                clicks % 3 + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, hit, point, clicks));
        let double = clicks == 2;
        // A click cancels Ctrl+B, as any key does.
        if self.desk.cancel_keys() {
            self.request_redraw();
        }
        // With a menu open, a click picks one of its items or, anywhere
        // else, just closes it.
        if let Some(menu) = self.menu.take() {
            if let Hit::Menu(index) = hit {
                self.menu_picked(menu.target, index);
            }
            self.request_redraw();
            return;
        }
        if let Hit::Finder(row) = hit {
            return self.clicked_in_finder(row, button, double);
        }
        // Clicks in a pane are the pane's: selecting, or for its program.
        if hit == Hit::Panes {
            self.clicked_in_pane(point, button, clicks);
            return;
        }
        if button == MouseButton::Right {
            return self.right_clicked(hit, point, &window);
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
            Hit::NewWorkspace => {
                self.desk.new_workspace();
                Ok(())
            }
            Hit::NewTab => {
                self.desk.new_tab();
                Ok(())
            }
            Hit::Panes | Hit::Menu(_) | Hit::Finder(_) | Hit::Nothing => Ok(()),
        };
        if let Err(err) = result {
            log::warn!("the desktop wouldn't move or resize the window: {err}");
        }
        self.update_title();
        self.request_redraw();
    }

    /// A click on the finder, which is its own: a row picks it, and a
    /// double click opens it.
    fn clicked_in_finder(&mut self, row: Option<usize>, button: MouseButton, double: bool) {
        if let Some(row) = row.filter(|_| button == MouseButton::Left) {
            self.desk.finder_click(row, double);
            self.send_outbox();
        }
        self.update_title();
        self.request_redraw();
    }

    /// A right click on `hit`, at `point`: a workspace's or tab's menu, or
    /// on the top bar, the desktop's menu for the window.
    fn right_clicked(&mut self, hit: Hit, point: [f32; 2], window: &Window) {
        let target = match hit {
            Hit::Workspace(workspace) => Some(MenuTarget::Workspace(workspace)),
            Hit::Tab(tab) => Some(MenuTarget::Tab(tab)),
            _ => None,
        };
        match target {
            Some(target) => {
                self.menu = Some(Menu { at: point, target });
                self.request_redraw();
            }
            None if hit == Hit::TopBar => {
                window.show_window_menu(PhysicalPosition::new(point[0], point[1]));
            }
            None => {}
        }
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

    /// Starts the config's Lua.
    pub(crate) fn start_lua(&mut self, start: crate::desk::LuaStart) {
        self.desk.start_lua(start);
    }

    /// Starts the tutor, with what it remembers.
    pub(crate) fn start_tutor(&mut self, start: crate::tutor::Start) {
        self.desk.start_tutor(start);
    }

    /// Sends what the desk has for the daemon, and tells the config's Lua
    /// what's changed.
    fn send_outbox(&mut self) {
        self.desk.sync_lua();
        // What was typed or clicked in a chat goes first, as it needs the
        // fonts, which the desk hasn't got, and may copy.
        if let Some(frame) = &mut self.frame
            && self.desk.settle_chats(&mut frame.fonts)
        {
            frame.window.request_redraw();
        }
        // Text copied, by a key, a click or an answer from the daemon.
        for (text, to) in self.desk.take_yanked() {
            if let Err(err) = copy_text(&mut self.clipboard, text, to) {
                log::warn!("copying failed: {err:#}");
            }
        }
        // The tutor's files, before the daemon's asked to open them. They're
        // a few small files in the scratch directory.
        for io in self.desk.take_tutor_io() {
            io.run();
        }
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
    /// Carries out the menu item at `index` for `target`.
    fn menu_picked(&mut self, target: MenuTarget, index: usize) {
        match (MENU_ITEMS.get(index).copied(), target) {
            (Some("Rename"), MenuTarget::Workspace(workspace)) => {
                self.desk.rename_workspace(workspace);
            }
            (Some("Rename"), MenuTarget::Tab(tab)) => self.desk.rename_tab(tab),
            (Some("Close"), MenuTarget::Workspace(workspace)) => {
                self.desk.close_workspace(workspace);
            }
            (Some("Close"), MenuTarget::Tab(tab)) => self.desk.close_tab(tab),
            _ => {}
        }
    }

    fn key_pressed(&mut self, event_loop: &ActiveEventLoop, event: &winit::event::KeyEvent) {
        // Any key closes a menu, and Esc does nothing else.
        if self.menu.take().is_some() {
            self.request_redraw();
            if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                return;
            }
        }
        if self.notice.is_some() {
            self.notice_key(event_loop, event);
            return;
        }
        let redraw = if is_paste(&event.logical_key, self.modifiers) {
            match paste_text(&mut self.clipboard, Copy::Clipboard) {
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
        self.open_links();
        self.update();
        self.update_title();
        if redraw {
            self.request_redraw();
        }
    }

    fn ime_event(&mut self, event: Ime) {
        // A chat shows what's being composed in its own box, cursor and
        // all.
        let for_chat = match &event {
            Ime::Preedit(text, cursor) if !text.is_empty() => Some(Some((text.clone(), *cursor))),
            Ime::Preedit(..) | Ime::Commit(_) | Ime::Disabled => Some(None),
            Ime::Enabled => None,
        };
        let committed = self.ime.event(event, Instant::now());
        if let Some(composing) = for_chat {
            self.composing_in_chat = self.desk.compose_in_chat(composing);
        }
        if let Some(text) = committed {
            self.type_text(&text);
            self.sync_ime();
        }
        self.request_redraw();
    }

    /// Types `text`, as the input method committed it, as if keyed.
    fn type_text(&mut self, text: &str) {
        if let Some(notice) = &mut self.notice {
            if let Notice::Ask { input, .. } = notice {
                input.push(text);
            }
            return;
        }
        let key = Key::Character(text.into());
        self.desk.key(&input::KeyPress {
            key: &key,
            text: Some(text),
            location: KeyLocation::Standard,
            code: None,
            modifiers: input::Modifiers::default(),
        });
        self.update_title();
    }

    /// Turns the input method on while keys type text, and off otherwise,
    /// as in normal mode, whose keys are commands, or while a notice asks
    /// for a password. The `ime` option can keep it off. Done after each
    /// key, not just each frame, so keys typed straight after Esc don't go
    /// to the input method.
    fn sync_ime(&mut self) {
        let Some(frame) = &self.frame else { return };
        let wanted =
            self.notice.is_none() && self.desk.options().ime && self.desk.text_goes_to().is_some();
        if let Some(allowed) = self.ime.allow(wanted) {
            frame.window.set_ime_allowed(allowed);
            self.ime_area = None;
            if !allowed {
                self.composing_in_chat = false;
                self.desk.compose_in_chat(None);
                self.request_redraw();
            }
        }
    }

    /// Turns the input method on or off as [`sync_ime`](Self::sync_ime)
    /// does, and tells it where the cursor is, at `area`, so its choices
    /// show beside it.
    fn update_ime(&mut self, area: Option<[f32; 4]>) {
        self.sync_ime();
        let Some(frame) = &self.frame else { return };
        let allowed = self.ime.allowed();
        if let Some([x, y, width, height]) = area.filter(|_| allowed && area != self.ime_area) {
            frame.window.set_ime_cursor_area(
                PhysicalPosition::new(x, y),
                PhysicalSize::new(width, height),
            );
            self.ime_area = area;
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
                    match paste_text(&mut self.clipboard, Copy::Clipboard) {
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
            Notice::OtherDaemon { protocol } if enter && *protocol <= PROTOCOL_VERSION => {
                self.notice = Some(Notice::Replacing {
                    upgrading: launch::can_upgrade(*protocol),
                });
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
        let Some(point) = self.pointer else {
            return;
        };
        #[allow(clippy::cast_possible_truncation)]
        let whole = whole as i64;
        if whole != 0 && matches!(self.hit(point), Hit::Finder(_)) {
            self.desk.finder_wheel(whole);
            self.request_redraw();
        } else if whole != 0 && self.desk.wheel(point, whole, self.modifiers) {
            self.request_redraw();
        }
        self.send_outbox();
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
        let drawn = if self.notice.is_some() {
            self.draw_notice(&chrome);
            // No input method while a notice asks for a password.
            self.sync_ime();
            return;
        } else {
            match self.draw_panes(&chrome) {
                Some(drawn) => drawn,
                None => return,
            }
        };
        // For measuring: `RUST_LOG=clackatoo_gui::frame=trace`.
        log::trace!(target: "clackatoo_gui::frame", "frame {}", started.elapsed().as_micros());
        // What the benchmarks time: a terminal on screen, not the notice
        // shown while connecting.
        if !drawn.is_empty() && !self.first_frame_drawn {
            self.first_frame_drawn = true;
            log::info!("first frame after {:.1?}", self.started.elapsed());
            // Not before, so opening the clipboard doesn't slow the start.
            if self.focused {
                self.hear_clipboard();
            }
        }
        self.desk.drawn(&drawn);
    }

    /// Tells the desk what's on the clipboard and the primary selection,
    /// which may have been copied elsewhere while the window didn't have
    /// the focus, for the editors' `"+` and `"*`. The editors can't wait for
    /// the clipboard as keys are typed, so it's asked now, on a thread of
    /// its own: an owner that's slow to answer, as a hung one is for
    /// seconds on X11, mustn't hold the window up. The answer comes as
    /// [`SessionEvent::Clipboard`]. One read at a time: a focus that comes
    /// back while one is under way waits for that one.
    fn hear_clipboard(&mut self) {
        use std::sync::atomic::Ordering;
        if self.reading_clipboard.swap(true, Ordering::AcqRel) {
            return;
        }
        self.desk.clipboard_asked();
        let (proxy, reading) = (self.session.proxy(), self.reading_clipboard.clone());
        let spawned = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let mut slot = None;
                let heard = [Copy::Clipboard, Copy::Primary]
                    .into_iter()
                    .filter_map(|from| match paste_text(&mut slot, from) {
                        Ok(text) => Some((from, text)),
                        Err(err) => {
                            log::debug!("reading the {from:?} failed: {err:#}");
                            None
                        }
                    })
                    .collect();
                reading.store(false, Ordering::Release);
                // The window has closed if this fails.
                let _ = proxy.send_event(SessionEvent::Clipboard(heard));
            });
        if let Err(err) = spawned {
            log::warn!("no thread to read the clipboard: {err}");
            self.reading_clipboard.store(false, Ordering::Release);
        }
    }

    /// Whether the window has rounded corners: where it can be
    /// see-through, unless it's maximised.
    fn rounded(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|frame| frame.transparent && !frame.window.is_maximized())
    }

    /// The window's background, rounded if the window is.
    fn window_fill(&self, geometry: &Geometry) -> Option<Fill> {
        self.frame.as_ref()?;
        Some(chrome::window_fill(geometry, self.rounded()))
    }

    /// Draws the notice in one pane filling the space between the bars.
    fn draw_notice(&mut self, chrome: &Chrome) {
        let Some(notice) = &self.notice else { return };
        let geometry = &chrome.geometry;
        let area = geometry.area;
        let placement = Placement::new(&single_pane(), &area);
        let tile = placement.panes[0].1;
        let Ok(size) = Size::new(tile.cols, tile.rows) else {
            return;
        };
        let lines = notice.lines(self.session.location().host(), usize::from(tile.cols));
        let screen = Screen::message(&lines, size);
        let rows: Vec<_> = screen.rows.iter().collect();
        let scenes = [Scene {
            origin: tile.origin,
            size: tile.cells_size(area.cell),
            rows: &rows,
            cursor: None,
            normal_mode: false,
            highlights: &[],
        }];
        let painted = chrome.paint(self.hover);
        let mut fills: Vec<Fill> = self.window_fill(geometry).into_iter().collect();
        fills.extend(painted.fills);
        fills.extend(chrome::paint_panes(geometry, &placement, None, None));
        let mut texts = painted.texts;
        texts.extend(painted.over_texts);
        if let Some(frame) = &mut self.frame {
            let transparent = frame.transparent;
            present(
                frame,
                &Picture {
                    scenes: &scenes,
                    fills: &fills,
                    over: &painted.over,
                    texts: &texts,
                    glyphs: &[],
                    over_glyphs: &[],
                    transparent,
                },
            );
        }
    }

    /// Draws the frame and the shown tab's panes. Returns the terminals
    /// drawn, or `None` if the frame wasn't shown.
    fn draw_panes(&mut self, chrome: &Chrome) -> Option<Vec<TerminalId>> {
        let (chats, chat_paint) = self.paint_chats(chrome);
        self.desk.settle_editors();
        self.desk.settle_explorers();
        self.desk.settle_boards();
        let geometry = &chrome.geometry;
        let desk = &self.desk;
        let shown = desk.shown()?;
        let tab = desk.shown_tab()?;
        let placement = Placement::new(&tab.layout, &geometry.area);
        let layout_panes = tab.layout.panes();
        let painted = chrome.paint(self.hover);
        let mut fills: Vec<Fill> = self.window_fill(geometry).into_iter().collect();
        fills.extend(painted.fills);
        fills.extend(chrome::paint_panes(
            geometry,
            &placement,
            Some(shown.pane),
            desk.pointed(),
        ));
        fills.extend(chat_paint.fills);
        let drawings = drawings(desk, self.focused, &placement, &layout_panes, shown.pane);
        let focused = drawings.iter().find(|drawing| drawing.focused);
        let cell = geometry.area.cell;
        let target = desk.text_goes_to();
        let preedit = (self.ime.preedit())
            .filter(|_| !self.composing_in_chat && target == Some(TextTarget::Pane));
        let (ime_area, composed) =
            focused.map_or((None, None), |drawing| compose(preedit, drawing, cell));
        let (bar_area, bar_underline) = match (target, self.frame.as_ref()) {
            (Some(TextTarget::Bar), Some(frame)) => {
                compose_in_bar(chrome, self.ime.preedit(), &frame.fonts.cell)
            }
            _ => (None, None),
        };
        let ime_area = bar_area.or(ime_area).or(chat_paint.ime);
        let composed_rows = composed
            .as_ref()
            .zip(focused)
            .map(|((row, cursor), drawing)| {
                let mut rows = drawing.rows.clone();
                rows[cursor.line] = row;
                rows
            });
        let scenes: Vec<Scene<'_>> = drawings
            .iter()
            .map(|drawing| {
                let composing = composed.as_ref().filter(|_| drawing.focused);
                Scene {
                    origin: drawing.tile.origin,
                    size: drawing.tile.cells_size(cell),
                    rows: composed_rows
                        .as_deref()
                        .filter(|_| drawing.focused)
                        .unwrap_or(&drawing.rows),
                    cursor: composing.map_or(drawing.cursor, |&(_, cursor)| Some(cursor)),
                    normal_mode: drawing.normal_mode,
                    highlights: &drawing.highlights,
                }
            })
            .collect();
        let mut texts = painted.texts;
        texts.extend(painted.over_texts);
        let mut over: Vec<Fill> = Vec::new();
        // The focused editor's panels by its cursor, under the palette and
        // menus: what's there, a call's signature, and the completion menu
        // with its documentation. Hidden while the palette or the finder
        // is open, as their text would mix.
        if chrome.palette.is_none()
            && chrome.finder.is_none()
            && let Some(editor) = desk.editor(shown.pane)
            && let Some(&(_, tile)) = (placement.panes.iter()).find(|(id, _)| *id == shown.pane)
        {
            for view in editor.panels() {
                chrome::paint_panel(geometry, tile.origin, &view, &mut over, &mut texts);
            }
        }
        over.extend(painted.over.into_iter().chain(chat_paint.over));
        over.extend(bar_underline);
        // Ctrl+B p: each pane's number, over it, to pick it by.
        if desk.numbering_panes() {
            let (fills, numbers) = chrome::paint_pane_numbers(geometry, &placement);
            over.extend(fills);
            texts.extend(numbers);
        }
        let transparent = self.frame.as_ref()?.transparent;
        let picture = Picture {
            scenes: &scenes,
            fills: &fills,
            over: &over,
            texts: &texts,
            glyphs: &chat_paint.glyphs,
            over_glyphs: &chat_paint.over_glyphs,
            transparent,
        };
        let frame = self.frame.as_mut()?;
        if !present(frame, &picture) {
            return None;
        }
        let drawn = drawings
            .iter()
            .filter_map(|drawing| drawing.terminal)
            .chain(chats)
            .collect();
        self.update_ime(ime_area);
        Some(drawn)
    }
}

impl App {
    /// Lays out and paints the shown tab's chats, which needs the fonts and
    /// each chat's view mutably, before the terminals are drawn. Returns
    /// the chats' terminals, and what to draw.
    fn paint_chats(&mut self, chrome: &Chrome) -> (Vec<TerminalId>, ChatPaint) {
        let mut all = ChatPaint::default();
        let mut chats = Vec::new();
        let (Some(frame), Some(shown), Some(tab)) = (
            self.frame.as_mut(),
            self.desk.shown(),
            self.desk.shown_tab(),
        ) else {
            return (chats, all);
        };
        let cell = chrome.geometry.area.cell;
        let placement = Placement::new(&tab.layout, &chrome.geometry.area);
        let layout_panes = tab.layout.panes();
        let tiles: Vec<(TerminalId, Tile, bool)> = (placement.panes.iter())
            .filter_map(|&(id, tile)| {
                let pane = layout_panes.iter().find(|pane| pane.id == id)?;
                Some((pane.content.terminal()?, tile, id == shown.pane))
            })
            .collect();
        for (terminal, tile, focused) in tiles {
            let Some(pane) = self.desk.pane_mut(terminal).filter(|pane| pane.is_chat()) else {
                continue;
            };
            // In insert mode, the cursor is in the box; in normal mode, in
            // the conversation.
            let caret = focused && self.focused;
            let size = tile.cells_size(cell);
            if let Some(paint) = pane.paint_chat(&mut frame.fonts, tile.origin, size, caret) {
                all.fills.extend(paint.fills);
                all.glyphs.extend(paint.glyphs);
                all.over.extend(paint.over);
                all.over_glyphs.extend(paint.over_glyphs);
                if focused {
                    all.ime = paint.ime;
                }
                chats.push(terminal);
            }
        }
        (chats, all)
    }
}

/// What each of the shown tab's terminals draws, as `placement` lays out
/// `panes`, `focused` having the focus, and the window having it if
/// `window_focused`.
fn drawings<'a>(
    desk: &'a Desk,
    window_focused: bool,
    placement: &Placement,
    panes: &[LayoutPane],
    focused: PaneId,
) -> Vec<Drawing<'a>> {
    let mut drawings = Vec::new();
    for &(id, tile) in &placement.panes {
        let Some(pane) = panes.iter().find(|pane| pane.id == id) else {
            continue;
        };
        let has_focus = id == focused;
        let terminal = match pane.content {
            Content::Terminal(terminal) => terminal,
            Content::Buffer(_) => {
                let Some(editor) = desk.editor(id) else {
                    continue;
                };
                let (rows, mut cursor) = editor.scene();
                if let Some(cursor) = &mut cursor
                    && !(has_focus && window_focused)
                {
                    cursor.style = CursorStyle::Hollow;
                }
                drawings.push(Drawing {
                    terminal: None,
                    tile,
                    rows,
                    cursor,
                    highlights: editor.highlights(),
                    normal_mode: has_focus && editor.mode != Mode::Insert,
                    focused: has_focus,
                });
                continue;
            }
            Content::Files(_) => {
                let Some(explorer) = desk.explorer(id) else {
                    continue;
                };
                let (rows, mut cursor) = explorer.scene();
                if let Some(cursor) = &mut cursor
                    && !(has_focus && window_focused)
                {
                    cursor.style = CursorStyle::Hollow;
                }
                drawings.push(Drawing {
                    terminal: None,
                    tile,
                    rows,
                    cursor,
                    highlights: explorer.highlights(),
                    normal_mode: has_focus,
                    focused: has_focus,
                });
                continue;
            }
            Content::Board(_) => {
                let Some(board) = desk.board(id) else {
                    continue;
                };
                let (rows, mut cursor) = board.scene();
                if let Some(cursor) = &mut cursor
                    && !(has_focus && window_focused)
                {
                    cursor.style = CursorStyle::Hollow;
                }
                drawings.push(Drawing {
                    terminal: None,
                    tile,
                    rows,
                    cursor,
                    highlights: board.highlights(),
                    normal_mode: has_focus,
                    focused: has_focus,
                });
                continue;
            }
        };
        let state = desk.pane(terminal);
        let normal = state.is_some_and(|state| state.mode != Mode::Insert);
        let Some(state) = state.filter(|state| state.screen().is_some()) else {
            continue;
        };
        let height = usize::from(tile.rows);
        let (rows, mut cursor) = state.scene(height);
        if let Some(cursor) = &mut cursor
            && !(has_focus && window_focused)
        {
            cursor.style = CursorStyle::Hollow;
        }
        drawings.push(Drawing {
            terminal: Some(terminal),
            tile,
            rows,
            cursor,
            highlights: state.highlights(height),
            normal_mode: has_focus && normal,
            focused: has_focus,
        });
    }
    drawings
}

/// The sizes the window's frame is worked out from, for `fonts` and
/// `padding` pixels of space between things.
#[allow(clippy::cast_precision_loss)]
fn metrics(fonts: &Fonts, padding: f32) -> Metrics {
    Metrics {
        cell: [fonts.cell.width as f32, fonts.cell.height as f32],
        ui_line: fonts.ui.height,
        padding,
    }
}

/// A layout of one pane, for a tile filling the whole space.
fn single_pane() -> Layout {
    Layout::Pane(LayoutPane {
        id: PaneId(0),
        content: Content::Terminal(TerminalId(0)),
    })
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

/// Where the input method's choices go, beside the focused pane's cursor,
/// and the cursor's row with `preedit`, the text the input method is
/// composing, over it, with where the cursor goes then.
#[allow(clippy::cast_precision_loss)]
fn compose(
    preedit: Option<&Preedit>,
    focused: &Drawing<'_>,
    cell: [f32; 2],
) -> (Option<[f32; 4]>, Option<(Row, SceneCursor)>) {
    let Some(cursor) = focused.cursor else {
        return (None, None);
    };
    let [left, top] = focused.tile.origin;
    let area = [
        left + cursor.col as f32 * cell[0],
        top + cursor.line as f32 * cell[1],
        cell[0],
        cell[1],
    ];
    let composed = preedit
        .zip(focused.rows.get(cursor.line))
        .map(|(preedit, row)| {
            let (row, col) = preedit.over(row, cursor.col);
            (row, SceneCursor { col, ..cursor })
        });
    (Some(area), composed)
}

/// The caret at the end of what's typed in the bottom bar.
const CARET: char = '▏';

/// While the bottom bar is typed in: where the input method's choices go,
/// at its caret, or where `preedit`, the text it's composing, starts, and
/// a line under that text, which the bar's text holds already. The text
/// is drawn in cells of `cell`'s size.
#[allow(clippy::cast_precision_loss)]
fn compose_in_bar(
    chrome: &Chrome,
    preedit: Option<&Preedit>,
    cell: &CellMetrics,
) -> (Option<[f32; 4]>, Option<Fill>) {
    let (Some([left, top]), Status::Typing { text, .. }) = (chrome.palette_text(), &chrome.status)
    else {
        return (None, None);
    };
    let Some(caret) = text.rfind(CARET) else {
        return (None, None);
    };
    let [width, height] = [cell.width as f32, cell.height as f32];
    let composed = preedit.map_or(0, |preedit| render::mono_width(&preedit.text));
    let before = render::mono_width(&text[..caret]).saturating_sub(composed);
    let start = left + before as f32 * width;
    let underline = (composed > 0).then(|| {
        Fill::solid(
            [
                start,
                top + cell.baseline + cell.underline_offset,
                composed as f32 * width,
                cell.stroke,
            ],
            Paint::Ink,
        )
    });
    (Some([start, top, width, height]), underline)
}

/// One pane, ready to draw.
struct Drawing<'a> {
    /// The terminal drawn, or `None` for a file.
    terminal: Option<TerminalId>,
    tile: Tile,
    rows: Vec<&'a Row>,
    cursor: Option<SceneCursor>,
    highlights: Vec<Highlight>,
    normal_mode: bool,
    focused: bool,
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
    /// Replacing it: upgrading it in place if `upgrading`.
    Replacing { upgrading: bool },
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
            Self::OtherDaemon { protocol } if *protocol > PROTOCOL_VERSION => vec![
                format!(
                    "The clackatoo daemon{on_host} is from a newer build than this window. \
                     It speaks protocol {protocol}, and this window speaks \
                     {PROTOCOL_VERSION}."
                ),
                "Open a window from the newer build instead.".into(),
                "  Esc    quit".into(),
            ],
            Self::OtherDaemon { protocol } if launch::can_upgrade(*protocol) => vec![
                format!(
                    "The clackatoo daemon{on_host} is from an older build. It speaks \
                     protocol {protocol}, and this window speaks {PROTOCOL_VERSION}."
                ),
                "Upgrading it moves it onto this build. Its terminals keep running.".into(),
                "  Enter  upgrade it\n  Esc    quit".into(),
            ],
            Self::OtherDaemon { protocol } => vec![
                format!(
                    "The clackatoo daemon{on_host} is from an older build. It speaks \
                     protocol {protocol}, and this window speaks {PROTOCOL_VERSION}."
                ),
                "It's too old to upgrade in place, so replacing it closes every terminal \
                 running in it, and hangs up their programs."
                    .into(),
                "  Enter  replace it\n  Esc    quit".into(),
            ],
            Self::Replacing { upgrading: true } => vec![format!("Upgrading the daemon{on_host}…")],
            Self::Replacing { upgrading: false } => {
                vec![format!("Replacing the daemon{on_host}…")]
            }
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
        .with_decorations(false)
        .with_transparent(true);
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

/// The text on the clipboard, or the primary selection.
fn paste_text(slot: &mut Option<Clipboard>, from: Copy) -> anyhow::Result<String> {
    let clipboard = clipboard(slot)?;
    Ok(match from {
        Copy::Clipboard => clipboard.get_text()?,
        Copy::Primary => clipboard
            .get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()?,
    })
}

/// Puts `text` on the clipboard, or the primary selection.
fn copy_text(slot: &mut Option<Clipboard>, text: String, to: Copy) -> anyhow::Result<()> {
    let clipboard = clipboard(slot)?;
    match to {
        Copy::Clipboard => clipboard.set_text(text)?,
        Copy::Primary => clipboard
            .set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text)?,
    }
    Ok(())
}

/// The button a pane's mouse handling knows `button` as, if it knows it.
fn mouse_button(button: MouseButton) -> Option<clackatoo_term::mouse::MouseButton> {
    match button {
        MouseButton::Left => Some(clackatoo_term::mouse::MouseButton::Left),
        MouseButton::Middle => Some(clackatoo_term::mouse::MouseButton::Middle),
        MouseButton::Right => Some(clackatoo_term::mouse::MouseButton::Right),
        _ => None,
    }
}

/// The padding around the grid in physical pixels.
fn padding(window: &Window) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let padding = (PADDING * window.scale_factor()).round() as u32;
    padding
}

impl ApplicationHandler<SessionEvent> for App {
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // A lesson goes with the window, and its scratch files with it.
        self.desk.tutor_exiting();
        self.send_outbox();
    }

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

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            if self.desk.format_if_due(Instant::now()) {
                self.send_outbox();
            }
            self.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // An editor may have something to ask, now typing has paused, or
        // a command to give up on, that waited too long for the grammar.
        if self.desk.tick_editors(Instant::now()) {
            self.send_outbox();
            self.request_redraw();
        }
        // A terminal's guess at the echo of a key may have waited long
        // enough to be taken as wrong.
        if self.desk.tick_panes(Instant::now()) {
            self.request_redraw();
        }
        // While the daemon is out of contact, the top bar counts the
        // seconds, a highlight on text just copied goes after a moment, an
        // editor asks its language server once typing pauses, and a chat
        // counts the seconds its agent has been working; otherwise,
        // nothing wakes the window but events.
        let counting = self
            .contact
            .as_ref()
            .and_then(Contact::silent_since)
            .map(|since| since + Duration::from_secs(since.elapsed().as_secs() + 1));
        let soonest = (counting.into_iter())
            .chain(self.desk.flash_ends())
            .chain(self.desk.grep_at())
            .chain(self.desk.format_at())
            .chain(self.desk.editor_wakes())
            .chain(self.desk.chat_wakes())
            .chain(self.desk.pane_wakes())
            .min();
        let flow = match soonest {
            Some(wake) => ControlFlow::WaitUntil(wake),
            None => ControlFlow::Wait,
        };
        event_loop.set_control_flow(flow);
    }
}

impl App {
    fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                log::debug!("resized to {size:?}");
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
                // Ctrl underlines the link under the mouse, and shows the
                // hand over it.
                if self.desk.modifiers_changed(self.modifiers) {
                    self.request_redraw();
                }
                if let Some(point) = self.pointer {
                    self.pointer_moved(point);
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if event.state.is_pressed() => {
                // While the input method composes, keys are its, and a key
                // it committed as well counts once. A modifier alone types
                // nothing, so it's none of the input method's business.
                if input::is_modifier(&event.logical_key)
                    || self.ime.key(event.text.as_deref(), Instant::now())
                {
                    self.key_pressed(event_loop, &event);
                    self.sync_ime();
                }
            }
            WindowEvent::Ime(ime) => self.ime_event(ime),
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
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => self.released(button),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.desk.window_focus(focused);
                if focused && self.first_frame_drawn {
                    self.hear_clipboard();
                }
                if !focused {
                    self.ime.reset();
                    self.composing_in_chat = false;
                    self.desk.compose_in_chat(None);
                }
                self.send_outbox();
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

    /// Hands the desk what the daemon says of file explorers, and of the
    /// file operations they asked for, or of boards.
    fn explorer_event(&mut self, event: Event) {
        let redraw = match event {
            Event::Board { .. } => self.desk.board_event(event),
            event => self.desk.explorer_event(event),
        };
        if redraw {
            self.update_title();
            self.request_redraw();
        }
    }

    /// Hands the desk what a program in a terminal asked of the clipboard
    /// (OSC 52).
    fn clipboard_event(&mut self, event: Event) {
        match event {
            Event::Clipboard {
                terminal,
                selection,
                text,
            } => self.desk.program_copied(terminal, selection, text),
            Event::ClipboardAsked {
                terminal,
                id,
                selection,
            } => self.desk.program_asked_clipboard(terminal, id, selection),
            _ => {}
        }
    }

    /// An agent asked to show a pane (see [`Desk::agent_shows`]).
    fn agent_shows(&mut self, pane: PaneId, at: Option<clackatoo_protocol::FileLocation>) {
        self.desk.agent_shows(pane, at);
        self.update_title();
        self.request_redraw();
    }

    #[allow(clippy::too_many_lines, reason = "one arm per event, each short")]
    fn handle_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Connected {
                sender,
                workspaces,
                contact,
                daemon_version,
            } => {
                self.notice = None;
                self.contact = contact;
                self.sender = Some(sender);
                let remote = self.session.location().host().is_some();
                self.desk.connected(workspaces, remote);
                let host = self.session.location().host();
                let news = self.updates.daemon_is(daemon_version.as_deref(), host);
                self.tell_updates(news);
                self.update_title();
                self.request_redraw();
            }
            SessionEvent::UpdateDue => {
                if self.desk.options().updatecheck {
                    updates::look(self.session.proxy());
                }
            }
            SessionEvent::NewRelease(version) => {
                let news = self.updates.newer_out(version);
                self.tell_updates(news);
            }
            SessionEvent::Updated(outcome) => {
                let said = match outcome {
                    Ok(said) => {
                        self.updates.updated();
                        said
                    }
                    Err(why) => why,
                };
                self.tell_updates(Some(said));
            }
            SessionEvent::Contact => self.request_redraw(),
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
            SessionEvent::Message(ServerMessage::Event(Event::Buffer { buffer, update })) => {
                if self.desk.buffer_changed(buffer, update) {
                    self.update_title();
                    self.request_redraw();
                }
            }
            SessionEvent::Message(ServerMessage::Event(Event::Installed { outcome, .. })) => {
                self.desk.installed(outcome);
                self.request_redraw();
            }
            SessionEvent::Message(ServerMessage::Event(Event::Found { id, found })) => {
                if self.desk.found(id, found) {
                    self.request_redraw();
                }
            }
            SessionEvent::Message(ServerMessage::Event(
                event @ (Event::Clipboard { .. } | Event::ClipboardAsked { .. }),
            )) => self.clipboard_event(event),
            SessionEvent::Message(ServerMessage::Event(
                event @ (Event::Files { .. } | Event::FileOp { .. } | Event::Board { .. }),
            )) => self.explorer_event(event),
            SessionEvent::Message(ServerMessage::Event(Event::Workspaces(workspaces))) => {
                self.desk.workspaces_changed(workspaces);
                self.update_title();
                self.request_redraw();
            }
            SessionEvent::Message(ServerMessage::Event(Event::Show { pane, at })) => {
                self.agent_shows(pane, at);
            }
            SessionEvent::Message(ServerMessage::Response(response)) => {
                let redraw = self.desk.answered(response);
                self.update_title();
                if redraw {
                    self.request_redraw();
                }
            }
            SessionEvent::Message(
                ServerMessage::Hello { .. } | ServerMessage::NotUpgrading { .. },
            ) => {}
            SessionEvent::Clipboard(heard) => {
                for (from, text) in heard {
                    self.desk.clipboard_heard(from, text);
                }
            }
            SessionEvent::Lua => {
                if self.desk.lua_said() {
                    self.update_title();
                    self.request_redraw();
                }
            }
            SessionEvent::Lost(reason) => {
                log::warn!("lost the daemon: {reason}");
                let was_connected = self.sender.take().is_some();
                self.desk.lost();
                self.contact = None;
                // A daemon still listening has just upgraded, or restarted:
                // carry on with it. Only once per connection, so one that
                // keeps hanging up is reported.
                if was_connected && self.session.daemon_listening() {
                    self.notice = Some(Notice::Connecting);
                    self.session.reconnect();
                } else {
                    self.notice = Some(Notice::Lost { reason });
                }
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

    // Whole pixels, worked out exactly.
    #[allow(clippy::float_cmp)]
    #[test]
    fn text_composed_in_the_bar_is_underlined_where_it_goes() {
        let metrics = Metrics {
            cell: [10.0, 20.0],
            ui_line: 18.0,
            padding: 8.0,
        };
        let cell = CellMetrics {
            width: 10,
            height: 20,
            baseline: 15.0,
            underline_offset: 2.0,
            strikeout_offset: 5.0,
            stroke: 1.0,
        };
        let chrome = |text: &str| {
            let buttons = ButtonLayout::default();
            Chrome::new(
                Geometry::new([800, 600], metrics),
                Contents {
                    workspaces: Vec::new(),
                    tabs: Vec::new(),
                    status: Status::Typing {
                        label: None,
                        text: text.into(),
                        choices: Vec::new(),
                    },
                    buttons: &buttons,
                    bottom_shown: true,
                    menu: None,
                    alert: None,
                    rounded: false,
                    finder: None,
                },
                &mut |_, _| 0.0,
            )
        };
        let preedit = Preedit {
            text: "日本".into(),
            cursor: None,
        };
        // The window has put the text being composed before the caret.
        let typing = chrome(":e 日本▏");
        let [left, top] = typing.palette_text().expect("a palette");
        let (area, underline) = compose_in_bar(&typing, Some(&preedit), &cell);
        assert_eq!(area, Some([left + 30.0, top, 10.0, 20.0]), "after `:e `");
        let underline = underline.expect("an underline");
        assert_eq!(underline.rect, [left + 30.0, top + 17.0, 40.0, 1.0]);
        // Nothing composed: the choices go at the caret, with no line.
        let typing = chrome(":e 日▏");
        let (area, underline) = compose_in_bar(&typing, None, &cell);
        assert_eq!(area, Some([left + 50.0, top, 10.0, 20.0]));
        assert!(underline.is_none());
        // Nothing typed in the bar.
        let buttons = ButtonLayout::default();
        let tabs = Chrome::new(
            Geometry::new([800, 600], metrics),
            Contents {
                workspaces: Vec::new(),
                tabs: Vec::new(),
                status: Status::Tabs {
                    mode: None,
                    note: None,
                    panes: 1,
                },
                buttons: &buttons,
                bottom_shown: true,
                menu: None,
                alert: None,
                rounded: false,
                finder: None,
            },
            &mut |_, _| 0.0,
        );
        assert_eq!(compose_in_bar(&tabs, Some(&preedit), &cell), (None, None));
    }

    #[test]
    fn typed_answers_never_show_in_debug_output() {
        let mut typed = Typed::default();
        typed.push("hunter2\n");
        assert_eq!(typed.0, "hunter2", "control characters are dropped");
        assert_eq!(format!("{typed:?}"), "Typed(..)");
    }
}
