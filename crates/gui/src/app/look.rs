//! Looking at the window without a desktop: the whole window, as it opens
//! on a desktop, connected to a daemon, but drawn into a picture in memory
//! instead of onto a screen, and given keys, clicks and the wheel by code.
//!
//! Everything but the surface is the window's own: the same [`App`] and
//! desk, handling the same events in the same order, laying out and
//! painting the same frames with the same fonts and renderer. Only
//! [`Target::Offscreen`] stands in for the window, and a channel for the
//! event loop: the caller turns the loop, with [`Window::settle`] and the
//! like.
//!
//! It's for people and agents to see what the window shows and how
//! quickly, as the `look` example does from a script, not part of
//! clackatoo itself; the window never draws offscreen otherwise.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use clackatoo_daemon::RuntimeDir;
use winit::event::{MouseButton, MouseScrollDelta};
use winit::keyboard::{Key, KeyLocation};

use super::{App, Frame, Gpu, Target, drawings};
use crate::fonts::Fonts;
use crate::input::{KeyPress, Modifiers};
use crate::layout::Placement;
use crate::render::{self, Picture as Drawn, Renderer};
use crate::session::{Location, Proxy, Session, SessionEvent};

/// The format pictures are drawn in. The window's own is BGRA where the
/// desktop offers it; both store the shader's values as they are, so the
/// colours are the same.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// How soon a time the window asked to wake at must be for settling to
/// wait for it (see [`Window::settle`]).
const WAKE_SOON: Duration = Duration::from_secs(2);

/// How far a drag moves the mouse between the events it makes, in points,
/// as a hand dragging steadily does.
const DRAG_STEP: f32 = 8.0;

/// A picture in memory that frames are drawn into, in place of a window.
#[derive(Debug)]
pub(super) struct Offscreen {
    texture: wgpu::Texture,
    /// How many pixels there are to a point.
    pub(super) scale: f64,
    /// Whether a frame was asked for, as a window asks the desktop.
    pub(super) redraw: Cell<bool>,
}

impl Offscreen {
    /// A picture `size` pixels big, at `scale` pixels to a point.
    fn new(device: &wgpu::Device, size: [u32; 2], scale: f64) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d {
                width: size[0].max(1),
                height: size[1].max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            texture,
            scale,
            redraw: Cell::new(true),
        }
    }

    /// How big the picture is, in pixels.
    pub(super) fn size(&self) -> [u32; 2] {
        [self.texture.width(), self.texture.height()]
    }

    /// Draws `picture` with `renderer`, as a window's frame is drawn.
    pub(super) fn render(
        &self,
        gpu: &Gpu,
        renderer: &mut Renderer,
        picture: &Drawn<'_>,
        fonts: &mut Fonts,
    ) {
        let view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        renderer.render(
            (&gpu.device, &gpu.queue),
            &view,
            self.size(),
            picture,
            fonts,
        );
    }
}

/// A frame drawn, as it was when drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// Its width and height, in pixels.
    pub size: [u32; 2],
    /// Its pixels, row by row from the top, four bytes each: red, green,
    /// blue and alpha.
    pub rgba: Vec<u8>,
}

/// What came of handing the window something, as frames show it: how
/// long it took to show, as [`Window::settle`] measured, from the first
/// key or click handed over since the last settling.
///
/// Each frame is read back from the GPU, to see whether it changed, and
/// that's counted in when it was done; and a debug build draws far slower
/// than a release build. So these are for comparing one step with
/// another, and the most each could take, not the latency a person
/// would see.
#[derive(Clone, Debug, Default)]
pub struct Timing {
    /// How long handing it over took, from the first key or click to the
    /// last, frames drawn meanwhile included.
    pub input: Duration,
    /// From the first key or click to the end of the first frame that
    /// looked different, if one did.
    pub first: Option<Duration>,
    /// From the first key or click to the end of the last frame that
    /// looked different before the window settled: when all it did was
    /// shown.
    pub done: Option<Duration>,
    /// How many frames were drawn since the first key or click.
    pub frames: u32,
    /// The longest any of those frames took to draw, on the CPU: what the
    /// window measures of a frame (`RUST_LOG=clackatoo_gui::frame=trace`).
    pub slowest: Duration,
    /// Whether the window settled: whether nothing changed for the quiet
    /// time asked before the time given ran out.
    pub settled: bool,
}

/// A frame drawn since the first input of a step.
#[derive(Clone, Copy, Debug)]
struct Drew {
    /// When it was finished, the GPU's work included.
    at: Instant,
    /// How long the window took to draw it, on the CPU.
    took: Duration,
    /// Whether it looked different from the frame before.
    changed: bool,
}

/// The window, drawn offscreen and driven by code (see the module's
/// documentation).
#[derive(Debug)]
pub struct Window {
    app: App,
    /// What the window's other threads send it, as its event loop's proxy
    /// would.
    events: mpsc::Receiver<SessionEvent>,
    /// When the window asked to wake next, as it asks its event loop.
    wake: Option<Instant>,
    /// The last frame drawn.
    last: Option<Picture>,
    /// When the first and last inputs since the last [`settle`] were
    /// handed over.
    ///
    /// [`settle`]: Window::settle
    inputs: Option<(Instant, Instant)>,
    /// The frames drawn since the first of those inputs.
    drew: Vec<Drew>,
    /// Whether the last turn of the loop woke the window at the time it
    /// asked to wake.
    woke: bool,
    /// Whether the window closed, as its close button closes it, or every
    /// workspace did.
    closed: bool,
}

impl Window {
    /// Opens the window on the daemon running in the runtime directory
    /// `runtime`, `size` points big at `scale` pixels to a point, as the
    /// window opens on a desktop, but drawn offscreen. It never starts a
    /// daemon: one this process started would be this program. With
    /// `config`, a config directory, it runs the `client/init.lua` there
    /// as the window does; without, no config, not even the one of
    /// whoever runs it.
    ///
    /// # Errors
    ///
    /// If there's no GPU or no font to draw with.
    pub fn open(
        runtime: PathBuf,
        size: [u32; 2],
        scale: f64,
        config: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        Self::open_with(
            runtime,
            size,
            scale,
            crate::tutor::Start::read(false),
            config,
        )
    }

    /// Opens the window as [`open`](Self::open) does, with the tutor
    /// starting from `tutor`.
    fn open_with(
        runtime: PathBuf,
        size: [u32; 2],
        scale: f64,
        tutor: crate::tutor::Start,
        config: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let started = Instant::now();
        let (sender, events) = mpsc::channel();
        let runtime = RuntimeDir::at(runtime);
        let session = Session::start(
            Location::Started(runtime.clone()),
            &runtime,
            Proxy::Channel(sender.clone()),
        );
        let mut app = App::new(started, None, session, String::new());
        let start = crate::options::Options::drawing_with(&crate::FontOptions::default());
        let font = start.font_options();
        app.start_options(start, Vec::new());
        // The config, if there's one, as `run` starts it.
        if let Some(dir) = config {
            app.start_lua(crate::desk::LuaStart(Box::new(move |options| {
                let sender = sender.clone();
                let wake: crate::lua::Wake = Box::new(move || {
                    let _ = sender.send(SessionEvent::Lua);
                });
                crate::lua::Lua::start(crate::lua::Source::Dir(dir.clone()), options.clone(), wake)
            })));
        }
        app.start_tutor(tutor);
        app.start_recall(crate::cmdline::recall::Start::read());
        // As the window opens: fonts loaded at a pixel a point, then
        // scaled to the screen's.
        let desktop = crate::desktop::Desktop::read();
        let mut fonts = Fonts::for_window(&font, &desktop)?;
        // As `run` does.
        crate::chat::warm_fonts(&fonts);
        app.desk.use_font(&mut fonts);
        app.buttons = desktop.buttons;
        fonts.set_scale(scale);
        let gpu = gpu()?;
        let renderer = Renderer::new(&gpu.device, FORMAT);
        let offscreen = Offscreen::new(&gpu.device, pixels(size, scale), scale);
        app.frame = Some(Frame {
            target: Target::Offscreen(offscreen),
            gpu,
            renderer,
            fonts,
            transparent: false,
        });
        app.fit_panes();
        Ok(Self {
            app,
            events,
            wake: None,
            last: None,
            inputs: None,
            drew: Vec::new(),
            woke: false,
            closed: false,
        })
    }

    /// Lets the window carry on by itself until it shows the daemon's
    /// workspaces, rather than a notice that it's connecting, or `time`
    /// has passed. Returns whether it did.
    pub fn connect(&mut self, time: Duration) -> bool {
        let until = Instant::now() + time;
        loop {
            self.turn(Instant::now());
            if self.app.notice.is_none() && self.app.desk.shown().is_some() {
                return true;
            }
            if self.closed || Instant::now() >= until {
                return false;
            }
            self.wait_for_event(until);
        }
    }

    /// Whether the window has closed.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// Makes the window `size` points big, as dragging its edge does.
    pub fn resize(&mut self, size: [u32; 2]) {
        let Some(frame) = &mut self.app.frame else {
            return;
        };
        let scale = frame.scale_factor();
        frame.target = Target::Offscreen(Offscreen::new(
            &frame.gpu.device,
            pixels(size, scale),
            scale,
        ));
        self.app.fit_panes();
        self.app.send_outbox();
    }

    /// Moves the window to a screen with `scale` pixels to a point,
    /// keeping its size in points.
    pub fn rescale(&mut self, scale: f64) {
        let Some(frame) = &mut self.app.frame else {
            return;
        };
        let [width, height] = frame.size();
        let old = frame.scale_factor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let points = [
            (f64::from(width) / old).round() as u32,
            (f64::from(height) / old).round() as u32,
        ];
        frame.target = Target::Offscreen(Offscreen::new(
            &frame.gpu.device,
            pixels(points, scale),
            scale,
        ));
        self.app.rescaled(scale);
        self.app.send_outbox();
    }

    /// Presses each key `notation` names in turn, in Vim's notation, as
    /// mappings name them: `:Explore<CR>`, `<Space>ff`, `<C-w>v`.
    ///
    /// # Errors
    ///
    /// If `notation` names a key that doesn't exist, before pressing any.
    pub fn keys(&mut self, notation: &str) -> Result<(), String> {
        let chords = crate::keymap::parse_keys(notation)?;
        for chord in chords {
            let kept = chord.kept();
            self.press_key(&kept.press());
        }
        Ok(())
    }

    /// Types `text`, a key a character, as a keyboard does.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let typed = ch.to_string();
            let key = Key::Character(typed.as_str().into());
            self.press_key(&KeyPress {
                key: &key,
                text: Some(&typed),
                location: KeyLocation::Standard,
                code: None,
                modifiers: Modifiers::default(),
            });
        }
    }

    /// Presses `press`, as the desktop says a key went down, with its
    /// modifiers held, then lets the window handle what's come meanwhile,
    /// as a person's keys come between other events.
    fn press_key(&mut self, press: &KeyPress<'_>) {
        self.input();
        self.app.modifiers = press.modifiers;
        self.app.key_event(press, false);
        self.app.send_outbox();
        self.app.modifiers = Modifiers::default();
        self.after_event();
        self.turn(Instant::now());
    }

    /// Moves the mouse to `point`, in points from the window's top left.
    pub fn move_to(&mut self, point: [f32; 2]) {
        self.input();
        let point = self.to_pixels(point);
        self.app.pointer_moved(point);
        self.app.send_outbox();
        self.turn(Instant::now());
    }

    /// Presses `button` where the mouse is.
    pub fn press(&mut self, button: MouseButton) {
        self.input();
        self.app.desk.user_active();
        self.app.clicked(button);
        self.app.send_outbox();
        self.after_event();
        self.turn(Instant::now());
    }

    /// Lets `button` go where the mouse is.
    pub fn release(&mut self, button: MouseButton) {
        self.input();
        self.app.released(button);
        self.app.send_outbox();
        self.turn(Instant::now());
    }

    /// Clicks `button` at `point`, in points.
    pub fn click(&mut self, point: [f32; 2], button: MouseButton) {
        self.move_to(point);
        self.press(button);
        self.release(button);
    }

    /// Drags with the left button from `from` to `to`, in points, in steps
    /// as a hand does.
    pub fn drag(&mut self, from: [f32; 2], to: [f32; 2]) {
        self.move_to(from);
        self.press(MouseButton::Left);
        let distance = (to[0] - from[0]).hypot(to[1] - from[1]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (distance / DRAG_STEP).ceil().max(1.0) as u32;
        for step in 1..=steps {
            #[allow(clippy::cast_precision_loss)]
            let along = step as f32 / steps as f32;
            self.move_to([
                from[0] + (to[0] - from[0]) * along,
                from[1] + (to[1] - from[1]) * along,
            ]);
        }
        self.release(MouseButton::Left);
    }

    /// Turns the wheel `notches` at `point`, in points: up for more than
    /// none, down for fewer, as a mouse's wheel does a notch at a time.
    pub fn scroll(&mut self, point: [f32; 2], notches: f32) {
        self.move_to(point);
        self.input();
        self.app.wheel(MouseScrollDelta::LineDelta(0.0, notches));
        self.app.send_outbox();
        self.turn(Instant::now());
    }

    /// Lets the window carry on by itself for `time`, handling what comes
    /// and drawing what it asks to, as its event loop would.
    pub fn wait(&mut self, time: Duration) {
        let until = Instant::now() + time;
        while !self.closed {
            let now = Instant::now();
            if now >= until {
                break;
            }
            self.turn(now);
            self.wait_for_event(until);
        }
    }

    /// Lets the window carry on by itself until it shows `text` somewhere
    /// (see [`text`](Self::text)), or `time` has passed. Returns whether
    /// it did.
    pub fn wait_for(&mut self, text: &str, time: Duration) -> bool {
        let until = Instant::now() + time;
        loop {
            self.turn(Instant::now());
            if self.text().contains(text) {
                return true;
            }
            if self.closed || Instant::now() >= until {
                return false;
            }
            // A while at most, as what's shown may change without an
            // event, as a chat's does once its fonts have laid it out.
            let soon = (Instant::now() + Duration::from_millis(20)).min(until);
            self.wait_for_event(soon);
        }
    }

    /// Lets the window carry on by itself until it settles: until no
    /// frame is waiting, nothing it draws has changed for `quiet`, and no
    /// time it asked to wake at is due soon, or `time` has passed. Returns
    /// how long what was handed to it since the last time took to show.
    ///
    /// A wake due soon, such as an editor's once typing pauses, may change
    /// what's shown, so it's waited for, once: one that comes round again
    /// and again, as a clock's, would otherwise hold settling up for ever.
    pub fn settle(&mut self, quiet: Duration, time: Duration) -> Timing {
        let until = Instant::now() + time;
        let mut calm_since = Instant::now();
        let mut woke_since = false;
        let settled = loop {
            self.woke = false;
            if self.turn(Instant::now()) {
                calm_since = Instant::now();
                woke_since = false;
            }
            woke_since |= self.woke;
            let now = Instant::now();
            let waking = !woke_since && self.wake.is_some_and(|wake| wake <= now + WAKE_SOON);
            let quiet_for = now.duration_since(calm_since) >= quiet;
            if !self.redraw_wanted() && quiet_for && !waking {
                break true;
            }
            if self.closed || now >= until {
                break false;
            }
            // Past the quiet time, only the wake is waited for, which
            // waiting does till anyway.
            let next = if quiet_for { until } else { calm_since + quiet };
            if self.wait_for_event(next.min(until)) {
                calm_since = Instant::now();
                woke_since = false;
            }
        };
        let timing = self.timing(settled);
        self.inputs = None;
        self.drew.clear();
        timing
    }

    /// The last frame drawn, if one has been.
    #[must_use]
    pub fn picture(&self) -> Option<&Picture> {
        self.last.as_ref()
    }

    /// The text the window shows now, as far as it can be read back: the
    /// panes' lines, the bars', finder's and palette's text, and what the
    /// chats drew, a line each.
    pub fn text(&mut self) -> String {
        let mut text = String::new();
        let Some(chrome) = self.app.chrome() else {
            return text;
        };
        let painted = chrome.paint(self.app.hover);
        for line in painted.texts.iter().chain(&painted.over_texts) {
            text.push_str(line.content);
            text.push('\n');
        }
        if let Some(notice) = &self.app.notice {
            for line in notice.lines(self.app.session.host(), usize::from(u16::MAX)) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        let desk = &self.app.desk;
        let (Some(shown), Some(tab)) = (desk.shown(), desk.shown_tab()) else {
            return text;
        };
        let placement = Placement::new(&tab.layout, &chrome.geometry.area);
        let panes = tab.layout.panes();
        for drawing in drawings(desk, self.app.focused, &placement, &panes, shown.pane) {
            for row in &drawing.rows {
                text.push_str(row.text().trim_end());
                text.push('\n');
            }
        }
        for pane in &panes {
            if let Some(chat) = (pane.content.terminal())
                .and_then(|terminal| desk.pane(terminal))
                .and_then(crate::pane::Pane::chat_text)
            {
                text.push_str(&chat);
                text.push('\n');
            }
        }
        text
    }

    /// Notes that something is being handed to the window. The first
    /// since the last settling starts what's timed.
    fn input(&mut self) {
        let now = Instant::now();
        if self.inputs.is_none() {
            self.drew.clear();
        }
        let first = self.inputs.map_or(now, |(first, _)| first);
        self.inputs = Some((first, now));
    }

    /// What follows a window event the window may close on, as its event
    /// loop does.
    fn after_event(&mut self) {
        if self.app.closing {
            self.closed = true;
        }
    }

    /// One turn of the event loop, at `now`, without waiting: what's come
    /// from the window's other threads handled, a wake that's due, a frame
    /// drawn if one's wanted, then what's due once all's handled. Returns
    /// whether anything changed: an event came, or a frame looked
    /// different.
    fn turn(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            self.session_event(event);
        }
        if self.wake.is_some_and(|wake| wake <= now) {
            self.wake = None;
            self.woke = true;
            self.app.woke(now);
        }
        if self.redraw_wanted() {
            changed |= self.draw();
        }
        self.wake = self.app.idle();
        changed
    }

    /// Hands the window `event`, as its event loop does.
    fn session_event(&mut self, event: SessionEvent) {
        if self.app.session_event(event) {
            self.closed = true;
        }
    }

    /// Waits until an event comes, the window asks to wake or a frame is
    /// wanted, or `until`. Returns whether an event came, which it hands
    /// the window.
    fn wait_for_event(&mut self, until: Instant) -> bool {
        if self.redraw_wanted() {
            return false;
        }
        let until = self.wake.map_or(until, |wake| wake.min(until));
        let time = until.saturating_duration_since(Instant::now());
        match self.events.recv_timeout(time) {
            Ok(event) => {
                self.session_event(event);
                true
            }
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => false,
        }
    }

    /// Whether the window has asked for a frame.
    fn redraw_wanted(&self) -> bool {
        match &self.app.frame {
            Some(Frame {
                target: Target::Offscreen(offscreen),
                ..
            }) => offscreen.redraw.get(),
            _ => false,
        }
    }

    /// Draws a frame, as the window does when the desktop asks, and reads
    /// it back. Returns whether it looked different from the last.
    fn draw(&mut self) -> bool {
        if let Some(Frame {
            target: Target::Offscreen(offscreen),
            ..
        }) = &self.app.frame
        {
            offscreen.redraw.set(false);
        }
        let started = Instant::now();
        self.app.draw();
        let took = started.elapsed();
        // As after any window event: what the frame acknowledged goes to
        // the daemon, which sends the next change only then.
        self.app.send_outbox();
        let Some(Frame {
            target: Target::Offscreen(offscreen),
            gpu,
            ..
        }) = &self.app.frame
        else {
            return false;
        };
        let picture = Picture {
            size: offscreen.size(),
            rgba: render::read_back((&gpu.device, &gpu.queue), &offscreen.texture),
        };
        let at = Instant::now();
        let changed = self.last.as_ref() != Some(&picture);
        self.drew.push(Drew { at, took, changed });
        self.last = Some(picture);
        changed
    }

    /// How long the inputs since the last settling took to show.
    fn timing(&self, settled: bool) -> Timing {
        let Some((first, last)) = self.inputs else {
            return Timing {
                settled,
                ..Timing::default()
            };
        };
        let since = |drew: &Drew| drew.at.saturating_duration_since(first);
        let mut changed = self.drew.iter().filter(|drew| drew.changed);
        Timing {
            input: last.duration_since(first),
            first: changed.next().map(since),
            done: self.drew.iter().rfind(|drew| drew.changed).map(since),
            frames: u32::try_from(self.drew.len()).unwrap_or(u32::MAX),
            slowest: (self.drew.iter().map(|drew| drew.took).max()).unwrap_or_default(),
            settled,
        }
    }

    /// `point`, in points, in the window's pixels.
    fn to_pixels(&self, point: [f32; 2]) -> [f32; 2] {
        let scale = self.app.frame.as_ref().map_or(1.0, Frame::scale_factor);
        #[allow(clippy::cast_possible_truncation)]
        let scale = scale as f32;
        [point[0] * scale, point[1] * scale]
    }
}

/// The GPU every window drawn offscreen in this process draws with. It's
/// opened once: opening and closing devices again and again ran
/// NVIDIA's Vulkan driver out of memory for its threads, and making them
/// on several threads at once can hang it.
fn gpu() -> anyhow::Result<Gpu> {
    static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
    GPU.get_or_init(|| {
        let gpu = Gpu::with_backends(wgpu::Backends::PRIMARY, None)
            .or_else(|_| Gpu::with_backends(wgpu::Backends::all(), None))
            .map_err(|err| format!("no GPU to draw with: {err:#}"))?;
        // In tests, a GPU error fails the test, as wgpu's own handler
        // makes it, rather than going in the log as the window's does.
        #[cfg(test)]
        gpu.device
            .on_uncaptured_error(std::sync::Arc::new(|error| panic!("GPU error: {error}")));
        Ok(gpu)
    })
    .clone()
    .map_err(|err| anyhow::anyhow!(err))
}

/// The device and queue of the GPU windows drawn offscreen draw with,
/// for the rendering tests to draw with too, rather than open another:
/// see [`gpu`].
#[cfg(test)]
pub(crate) fn shared_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    gpu().ok().map(|gpu| (gpu.device, gpu.queue))
}

/// `size`, in points, in pixels at `scale` pixels to a point.
fn pixels(size: [u32; 2], scale: f64) -> [u32; 2] {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    size.map(|side| (f64::from(side) * scale).round().max(1.0) as u32)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use clackatoo_daemon::{AgentConfig, Daemon, DaemonConfig, RuntimeDir};

    use super::Window;

    /// Long enough for anything here, on a busy machine.
    const PATIENCE: Duration = Duration::from_secs(20);

    #[test]
    fn a_window_drawn_offscreen_shows_what_its_shell_prints_and_how_soon() {
        if super::gpu().is_err() {
            eprintln!("no GPU: skipped");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimeDir::at(dir.path().join("run"));
        std::fs::create_dir_all(runtime.path()).unwrap();
        let config = DaemonConfig {
            shell: "/bin/sh".into(),
            home: dir.path().to_owned(),
            shell_integration: None,
            agents: AgentConfig::default(),
            mcp: None,
            udp_ports: None,
            languages: None,
            servers: None,
            declined: None,
            lsp_trace: None,
            boards: None,
            recovery: None,
            browser: None,
            web: None,
            trash: None,
            workspaces: None,
            keep_gate: None,
            ..DaemonConfig::from_env()
        };
        let mut daemon = Daemon::bind(&runtime.socket_path(), config).unwrap();
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().unwrap()));

        // Two pixels to a point, as on a HiDPI screen, and a tutor that
        // works nowhere.
        let mut window = Window::open_with(
            runtime.path().to_owned(),
            [640, 400],
            2.0,
            crate::tutor::Start::default(),
            None,
        )
        .unwrap();
        assert!(window.connect(PATIENCE), "{}", window.text());
        window.settle(Duration::from_millis(100), PATIENCE);

        // What's typed reaches the shell, and what it prints is drawn.
        window.type_text("echo look-$((40 + 2))");
        window.keys("<CR>").unwrap();
        assert!(window.wait_for("look-42", PATIENCE), "{}", window.text());
        let timing = window.settle(Duration::from_millis(100), PATIENCE);
        assert!(timing.settled);
        assert!(timing.frames > 0 && timing.first.is_some(), "{timing:?}");
        assert!(timing.first <= timing.done, "{timing:?}");
        let picture = window.picture().unwrap();
        assert_eq!(picture.size, [1280, 800]);
        assert_eq!(picture.rgba.len(), 1280 * 800 * 4);
        // Not one colour: there's text, and bars.
        assert!(
            picture
                .rgba
                .chunks(4)
                .any(|pixel| pixel != &picture.rgba[..4])
        );

        // A key that isn't in the notation is refused.
        assert!(window.keys("<NoSuchKey>").is_err());

        // The mouse goes where it's told in points, at two pixels to one:
        // a right click on the workspace's name, in the top bar, opens
        // its menu.
        assert!(!window.text().contains("Rename"));
        window.click([12.0, 20.0], winit::event::MouseButton::Right);
        let clicked = window.settle(Duration::from_millis(100), PATIENCE);
        assert!(window.text().contains("Rename"), "{}", window.text());
        // Timed from the press, so the menu it opened counts, though the
        // release came after it.
        assert!(clicked.frames > 0 && clicked.first.is_some(), "{clicked:?}");
        window.keys("<Esc>").unwrap();

        // Resized and rescaled, it draws at the new size.
        window.resize([320, 200]);
        window.settle(Duration::from_millis(100), PATIENCE);
        assert_eq!(window.picture().unwrap().size, [640, 400]);
        window.rescale(1.0);
        window.settle(Duration::from_millis(100), PATIENCE);
        assert_eq!(window.picture().unwrap().size, [320, 200]);

        // Settling again with nothing handed over says nothing took time.
        let idle = window.settle(Duration::from_millis(50), PATIENCE);
        assert_eq!((idle.first, idle.frames), (None, 0), "{idle:?}");
        drop(window);

        // Given a config, it runs its `init.lua`, as `look`'s `config`
        // step has it do; given none, as above, it runs nobody's.
        let config = dir.path().join("config");
        std::fs::create_dir_all(config.join("client")).unwrap();
        std::fs::write(config.join("client/init.lua"), "client.opt.scrolloff = 7\n").unwrap();
        let mut window = Window::open_with(
            runtime.path().to_owned(),
            [320, 200],
            1.0,
            crate::tutor::Start::default(),
            Some(config),
        )
        .unwrap();
        assert!(window.connect(PATIENCE), "{}", window.text());
        let until = std::time::Instant::now() + PATIENCE;
        while window.app.desk.options().scrolloff != 7 && std::time::Instant::now() < until {
            window.settle(Duration::from_millis(50), PATIENCE);
        }
        assert_eq!(window.app.desk.options().scrolloff, 7);

        drop(window);
        shutdown.shutdown();
        thread.join().unwrap();
    }
}
