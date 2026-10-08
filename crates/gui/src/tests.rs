//! Rendering a known screen offscreen, to check what the renderer draws.

use clackatoo_protocol::{Size, View};
use clackatoo_term::{Config, Terminal};

use clackatoo_protocol::{Axis, Content, Layout, Pane, PaneId, TabId, TerminalId, WorkspaceId};

use crate::chrome::{self, Chrome, Contents, Geometry, Hit, Metrics, ModeName, Status};
use crate::desktop::ButtonLayout;
use crate::fonts::{FontOptions, Fonts};
use crate::layout::Placement;
use crate::render::{CursorStyle, Highlight, HighlightKind, Picture, Renderer, Scene, SceneCursor};
use crate::screen::Screen;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The GPU device every test shares, or `None` on machines without one.
/// It's made once: tests making and dropping devices of their own at the
/// same time sometimes hung NVIDIA's Vulkan driver.
fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    static GPU: std::sync::OnceLock<Option<(wgpu::Device, wgpu::Queue)>> =
        std::sync::OnceLock::new();
    GPU.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    })
    .clone()
}

/// Renders `output` played into a terminal of `cols` × `rows`, and returns
/// the image's size and RGBA pixels.
fn render(
    cols: u16,
    rows: u16,
    output: &str,
    scale: f64,
    (normal_mode, highlights): (bool, &[Highlight]),
) -> Option<([u32; 2], Vec<u8>)> {
    let gpu = gpu()?;
    let screen = screen(cols, rows, output);
    let mut fonts = fonts(scale);
    let padding = 4;
    let size = [
        u32::from(cols) * fonts.cell.width + 2 * padding,
        u32::from(rows) * fonts.cell.height + 2 * padding,
    ];
    let rows: Vec<_> = screen.rows.iter().collect();
    let cursor = screen.state.cursor;
    #[allow(clippy::cast_precision_loss)]
    let scene = Scene {
        origin: [padding as f32; 2],
        size: [
            (size[0] - 2 * padding) as f32,
            (size[1] - 2 * padding) as f32,
        ],
        rows: &rows,
        cursor: Some(SceneCursor {
            line: usize::from(cursor.line),
            col: usize::from(cursor.col),
            style: CursorStyle::Block,
        }),
        normal_mode,
        highlights,
    };
    let picture = Picture {
        scenes: &[scene],
        fills: &[],
        over: &[],
        texts: &[],
        transparent: false,
    };
    Some((size, draw(&gpu, size, &picture, &mut fonts)))
}

/// `output` played into a terminal of `cols` × `rows`, as the window shows
/// it.
fn screen(cols: u16, rows: u16, output: &str) -> Screen {
    let mut terminal = Terminal::new(Size::new(cols, rows).unwrap(), &Config::default());
    terminal.advance(output.as_bytes());
    Screen::new(&View::capture(&terminal))
}

fn fonts(scale: f64) -> Fonts {
    let options = FontOptions {
        family: std::env::var("CLACKATOO_TEST_FONT").ok(),
        size: 12.0,
    };
    let mut fonts = Fonts::load(&options, scale).expect("a monospace font");
    fonts.load_ui(None, 11.0, scale);
    fonts
}

/// Draws `picture` in an image of `size`, and returns its RGBA pixels.
fn draw(
    (device, queue): &(wgpu::Device, wgpu::Queue),
    size: [u32; 2],
    picture: &Picture<'_>,
    fonts: &mut Fonts,
) -> Vec<u8> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = Renderer::new(device, FORMAT);
    renderer.render((device, queue), &view, size, picture, fonts);

    // Rows of a texture copy must be padded to 256 bytes.
    let row_bytes = size[0] * 4;
    let padded = row_bytes.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded * size[1]),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size[1]),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, |result| result.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let mapped = buffer.slice(..).get_mapped_range().expect("mapped");
    mapped
        .chunks(padded as usize)
        .flat_map(|row| &row[..row_bytes as usize])
        .copied()
        .collect()
}

/// Saves an image under `target/gui-snapshots/`, for a person to look at.
fn save(name: &str, size: [u32; 2], pixels: &[u8]) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/gui-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), size[0], size[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(pixels)
        .unwrap();
}

#[test]
fn renders_text_colours_and_the_cursor() {
    let output = "plain \x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m \x1b[9mstruck\x1b[0m\r\n\
        \x1b[31mred \x1b[32mgreen \x1b[34mblue \x1b[38;2;255;128;0mrgb\x1b[0m \x1b[7minverse\x1b[0m\r\n\
        \x1b[41m red bg \x1b[0m 字符 e\u{301} 😀 👩\u{200D}💻 ok\r\n\
        box: ┌─┐ │ │ └─┘  arrows: → ← ↑ ↓\r\n$ ";
    let Some((size, pixels)) = render(48, 6, output, 2.0, (false, &[])) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    save("text", size, &pixels);
    // Something other than the background was drawn.
    let background = &pixels[..4];
    assert!(pixels.chunks(4).any(|pixel| pixel != background));
}

#[test]
fn box_drawing_joins_across_rows() {
    let output = "┌──┬──┐ ╭──╮ ┏━━┓ ╔══╦═╗\r\n\
        │ab│cd│ │  │ ┃  ┃ ║  ║ ║\r\n\
        ├──┼──┤ ╰──╯ ┗━━┛ ╠══╬═╣\r\n\
        └──┴──┘ ▁▂▃▄▅▆▇█ ░▒▓ ▌▐ ▖▗▘▝ ╚══╩═╝";
    let Some((size, pixels)) = render(40, 5, output, 2.0, (true, &[])) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    save("boxes", size, &pixels);
}

#[test]
fn selections_and_matches_are_highlighted() {
    let output = "selected text on\r\ntwo lines, and a match";
    let highlights = [
        Highlight {
            line: 0,
            cols: 9..16,
            kind: HighlightKind::Selection,
        },
        Highlight {
            line: 1,
            cols: 0..9,
            kind: HighlightKind::Selection,
        },
        Highlight {
            line: 1,
            cols: 17..22,
            kind: HighlightKind::Match,
        },
    ];
    let Some((size, pixels)) = render(30, 3, output, 2.0, (true, &highlights)) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    save("highlights", size, &pixels);
}

/// A window with room for two tiles of 20 x 4 cells side by side, with
/// its frame laid out and its tiles placed.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn two_pane_window(
    fonts: &mut Fonts,
    status: Status,
    menu: Option<([f32; 2], Vec<String>)>,
    alert: Option<String>,
) -> ([u32; 2], Chrome, Placement) {
    let metrics = Metrics {
        cell: [fonts.cell.width as f32, fonts.cell.height as f32],
        ui_line: fonts.ui.height,
        padding: 8.0,
    };
    // One pane of 20 columns, the line, and another, inset alike.
    let [width, height] = Geometry::window_size(20, 4, metrics);
    let line = (metrics.padding / 4.0).round();
    let size = [(2.0 * width + line) as u32, height as u32];
    let geometry = Geometry::new(size, metrics);
    let buttons = ButtonLayout::default();
    let chrome = Chrome::new(
        geometry,
        Contents {
            workspaces: vec![
                (WorkspaceId(1), "clackatoo".into(), 2, true),
                (WorkspaceId(2), "peios".into(), 1, false),
            ],
            tabs: vec![
                (TabId(1), "1 zsh".into(), true),
                (TabId(2), "2 htop".into(), false),
            ],
            status,
            buttons: &buttons,
            bottom_shown: true,
            menu,
            alert,
            rounded: false,
        },
        &mut |text, bold| fonts.ui_width(text, bold),
    );
    let pane = |id| {
        Layout::Pane(Pane {
            id: PaneId(id),
            content: Content::Terminal(TerminalId(id)),
        })
    };
    let layout = Layout::Split {
        axis: Axis::LeftToRight,
        parts: vec![pane(1), pane(2)],
        shares: vec![1, 1],
    };
    (size, chrome, Placement::new(&layout, &geometry.area))
}

/// Draws `chrome` and `placement`'s tiles, with `screens` on them, the
/// second focused in normal mode.
fn draw_window(
    gpu: &(wgpu::Device, wgpu::Queue),
    fonts: &mut Fonts,
    window: &([u32; 2], Chrome, Placement),
    screens: [&Screen; 2],
) -> Vec<u8> {
    draw_window_shaped(gpu, fonts, window, screens, (false, false))
}

/// Draws as [`draw_window`] does, in a window with rounded corners and
/// see-through beyond them if `rounded`, and with the panes' numbers over
/// them if `numbered`, as while Ctrl+B p waits.
fn draw_window_shaped(
    gpu: &(wgpu::Device, wgpu::Queue),
    fonts: &mut Fonts,
    (size, chrome, placement): &([u32; 2], Chrome, Placement),
    screens: [&Screen; 2],
    (rounded, numbered): (bool, bool),
) -> Vec<u8> {
    let geometry = chrome.geometry;
    let rows: Vec<Vec<_>> = screens
        .iter()
        .map(|screen| screen.rows.iter().collect())
        .collect();
    let scenes: Vec<Scene<'_>> = placement
        .panes
        .iter()
        .zip(&rows)
        .enumerate()
        .map(|(index, ((_, tile), rows))| Scene {
            origin: tile.origin,
            size: tile.cells_size(geometry.area.cell),
            rows,
            cursor: None,
            normal_mode: index == 1,
            highlights: &[],
        })
        .collect();
    let painted = chrome.paint(Hit::Workspace(WorkspaceId(2)));
    let mut fills = vec![chrome::window_fill(&geometry, rounded)];
    fills.extend(painted.fills);
    let focused = placement.panes.get(1).map(|&(id, _)| id);
    fills.extend(chrome::paint_panes(&geometry, placement, focused));
    let mut texts = painted.texts;
    texts.extend(painted.over_texts);
    let mut over = painted.over;
    if numbered {
        let (fills, numbers) = chrome::paint_pane_numbers(&geometry, placement);
        over.extend(fills);
        texts.extend(numbers);
    }
    let picture = Picture {
        scenes: &scenes,
        fills: &fills,
        over: &over,
        texts: &texts,
        transparent: rounded,
    };
    draw(gpu, *size, &picture, fonts)
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn panes_are_drawn_side_by_side_between_the_bars() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let window = two_pane_window(
        &mut fonts,
        Status::Tabs {
            mode: Some(ModeName::Normal),
            note: Some("↑3".into()),
            panes: 2,
        },
        None,
        None,
    );
    // Each terminal is wider than its tile, as before a resize lands: the
    // left one's long line must stop at its tile's edge.
    let left = screen(30, 4, "left pane, with a line too long for it\r\n$ ");
    let right = screen(20, 4, "right pane\r\n$ ");
    let pixels = draw_window(&gpu, &mut fonts, &window, [&left, &right]);
    let (size, chrome, placement) = &window;
    save("panes", *size, &pixels);

    let pixel = |[x, y]: [f32; 2]| {
        let index = (y as usize * size[0] as usize + x as usize) * 4;
        &pixels[index..index + 3]
    };
    let brightness = |rgb: &[u8]| rgb.iter().map(|&c| u32::from(c)).sum::<u32>();
    // The workspace shown, and the one under the mouse, sit on soft
    // backgrounds, lighter than the backdrop.
    for item in &chrome.workspaces {
        let inside = pixel([item.rect.x + 6.0, item.rect.y + item.rect.height - 4.0]);
        let beside = pixel([item.rect.x - 3.0, item.rect.y + item.rect.height - 4.0]);
        assert!(
            brightness(inside) > brightness(beside) + 20,
            "{} has a background",
            item.text
        );
    }
    // The panes are the terminal's background, darker than the bars.
    let right_tile = placement.panes[1].1.rect;
    let bar = pixel([size[0] as f32 / 2.0, 2.0]);
    let pane = pixel([right_tile.x + 4.0, right_tile.y + right_tile.height - 4.0]);
    assert_eq!(pane, [0x0c, 0x0e, 0x10]);
    assert!(brightness(bar) > brightness(pane));
    // A line in clackatoo's colour runs round the focused pane, over the
    // line between the panes.
    let edge = pixel([right_tile.x - 1.0, right_tile.y + right_tile.height / 2.0]);
    assert!(
        edge[0] > edge[2] + 30,
        "the focused pane's edge is gold: {edge:?}"
    );
    // The long line stopped at its pane's edge: nothing bright in the
    // right pane before its own text starts.
    let spilled = (0..fonts.cell.height).any(|y| {
        let rgb = pixel([
            right_tile.x + 4.0,
            placement.panes[0].1.origin[1] + y as f32,
        ]);
        rgb.iter().any(|&c| c > 0x60)
    });
    assert!(!spilled, "the cut-off line spilled out of its pane");
    // The mode is named on its colour at the bottom left.
    let (mode, rect) = chrome.mode.expect("a mode");
    assert_eq!(mode, ModeName::Normal);
    assert_eq!(pixel([rect.x + 2.0, rect.y + 2.0]), [0xe8, 0xc2, 0x5a]);
}

#[test]
fn the_palette_is_drawn_over_the_panes() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let window = two_pane_window(
        &mut fonts,
        Status::Typing {
            label: Some("Workspace".into()),
            text: "o▏".into(),
            choices: vec![
                ("clackatoo".into(), false),
                ("dotfiles".into(), true),
                ("homelab".into(), false),
            ],
        },
        None,
        None,
    );
    let left = screen(20, 4, "left\r\n$ ");
    let right = screen(20, 4, "right\r\n$ ");
    let pixels = draw_window(&gpu, &mut fonts, &window, [&left, &right]);
    save("palette", window.0, &pixels);
    assert!(window.1.palette.is_some());
}

#[test]
fn a_menu_opens_where_it_was_asked_for() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let probe = two_pane_window(&mut fonts, Status::tabs(), None, None);
    let workspace = probe.1.workspaces[0].rect;
    let at = [workspace.x + 10.0, workspace.y + workspace.height];
    let window = two_pane_window(
        &mut fonts,
        Status::tabs(),
        Some((at, vec!["Rename".into(), "Close".into()])),
        None,
    );
    let left = screen(20, 4, "left\r\n$ ");
    let right = screen(20, 4, "right\r\n$ ");
    let pixels = draw_window(&gpu, &mut fonts, &window, [&left, &right]);
    save("menu", window.0, &pixels);
    let (rect, items) = window.1.menu.clone().expect("a menu");
    assert_eq!((rect.x, rect.y), (at[0].round(), at[1].round()));
    assert_eq!(items.len(), 2);
    assert_eq!(
        window
            .1
            .hit([items[1].1.x + 4.0, items[1].1.y + 4.0], false),
        Hit::Menu(1)
    );
}

#[test]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn an_alert_is_drawn_in_the_top_bar() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let window = two_pane_window(
        &mut fonts,
        Status::tabs(),
        None,
        Some("no contact with box for 4 s".into()),
    );
    let left = screen(20, 4, "left\r\n$ ");
    let right = screen(20, 4, "right\r\n$ ");
    let pixels = draw_window(&gpu, &mut fonts, &window, [&left, &right]);
    save("alert", window.0, &pixels);
    let (_, rect) = window.1.alert.clone().expect("an alert");
    // Inside the pill, past its rounded corner, it's redder than the bar
    // just outside it.
    let at = |x: f32, y: f32| {
        let index = (y as usize * window.0[0] as usize + x as usize) * 4;
        [pixels[index], pixels[index + 1], pixels[index + 2]]
    };
    let inside = at(rect.x + rect.height / 2.0, rect.y + 2.0);
    let outside = at(rect.x - 4.0, rect.y + 2.0);
    let rise = |channel: usize| i32::from(inside[channel]) - i32::from(outside[channel]);
    assert!(
        rise(0) > rise(1) + 10 && rise(0) > rise(2) + 10,
        "not tinted red: {inside:?} against {outside:?}"
    );
}

#[test]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn nothing_is_drawn_past_a_rounded_windows_corners() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let window = two_pane_window(
        &mut fonts,
        Status::Tabs {
            mode: Some(ModeName::Insert),
            note: None,
            panes: 2,
        },
        None,
        None,
    );
    let mut window = window;
    window.1.rounded = true;
    let left = screen(20, 4, "left\r\n$ ");
    let right = screen(20, 4, "right\r\n$ ");
    let pixels = draw_window_shaped(&gpu, &mut fonts, &window, [&left, &right], (true, true));
    save("rounded", window.0, &pixels);
    let [width, height] = window.0;
    let alpha = |x: u32, y: u32| pixels[((y * width + x) * 4 + 3) as usize];
    for (x, y) in [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
    ] {
        assert_eq!(alpha(x, y), 0, "the corner at {x}, {y} is see-through");
    }
}
