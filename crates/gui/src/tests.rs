//! Rendering a known screen offscreen, to check what the renderer draws.

use clackatoo_protocol::{Size, View};
use clackatoo_term::{Config, Terminal};

use clackatoo_protocol::{Axis, Content, Layout, Pane, PaneId, TabId, TerminalId, WorkspaceId};

use crate::chrome::{Chrome, Contents, Geometry, Hit, Metrics, Status};
use crate::desktop::ButtonLayout;
use crate::fonts::{FontOptions, Fonts};
use crate::layout::Placement;
use crate::render::{
    CursorStyle, Fill, Highlight, HighlightKind, Paint, Picture, Renderer, Scene, SceneCursor,
};
use crate::screen::Screen;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A GPU device, or `None` on machines without one.
fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
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

/// A window with room for two cards of 20 x 4 cells side by side, with
/// its frame laid out and its cards placed.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn two_pane_window(fonts: &mut Fonts, status: Status) -> ([u32; 2], Chrome, Placement) {
    let metrics = Metrics {
        cell: [fonts.cell.width as f32, fonts.cell.height as f32],
        ui_line: fonts.ui.height,
        padding: 8.0,
    };
    // One card of 20 columns, the gap, and another, inset alike.
    let gap = 2.0 * metrics.padding;
    let [width, height] = Geometry::window_size(20, 4, metrics);
    let size = [(2.0 * width - 2.0 * gap) as u32 + gap as u32, height as u32];
    let geometry = Geometry::new(size, metrics);
    let buttons = ButtonLayout::default();
    let chrome = Chrome::new(
        geometry,
        Contents {
            workspaces: vec![
                (WorkspaceId(1), "clackatoo".into(), true),
                (WorkspaceId(2), "peios".into(), false),
            ],
            tabs: vec![
                (TabId(1), "1 zsh".into(), true),
                (TabId(2), "2 htop".into(), false),
            ],
            status,
            buttons: &buttons,
            bottom_shown: true,
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
    };
    (size, chrome, Placement::new(&layout, &geometry.area))
}

/// Draws `chrome` and `placement`'s cards, with `screens` on them, the
/// second focused in normal mode.
fn draw_window(
    gpu: &(wgpu::Device, wgpu::Queue),
    fonts: &mut Fonts,
    (size, chrome, placement): &([u32; 2], Chrome, Placement),
    screens: [&Screen; 2],
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
        .map(|(index, ((_, card), rows))| Scene {
            origin: card.origin,
            size: card.cells_size(geometry.area.cell),
            rows,
            cursor: None,
            normal_mode: index == 1,
            highlights: &[],
        })
        .collect();
    let painted = chrome.paint(Hit::Workspace(WorkspaceId(2)));
    let mut fills = painted.fills;
    for (index, (_, card)) in placement.panes.iter().enumerate() {
        let edge = if index == 1 {
            Paint::Accent
        } else {
            Paint::Edge
        };
        fills.push(
            Fill::rounded(card.rect.to_array(), Paint::Card, geometry.radius)
                .bordered(geometry.edge, edge),
        );
    }
    let mut texts = painted.texts;
    texts.extend(painted.over_texts);
    let picture = Picture {
        scenes: &scenes,
        fills: &fills,
        over: &painted.over,
        texts: &texts,
        transparent: false,
    };
    draw(gpu, *size, &picture, fonts)
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn panes_are_drawn_on_cards_between_the_bars() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let window = two_pane_window(
        &mut fonts,
        Status::Tabs {
            note: Some("NORMAL".into()),
        },
    );
    // Each terminal is wider than its card, as before a resize lands: the
    // left one's long line must stop at its card's edge.
    let left = screen(30, 4, "left pane, with a line too long for it\r\n$ ");
    let right = screen(20, 4, "right pane\r\n$ ");
    let pixels = draw_window(&gpu, &mut fonts, &window, [&left, &right]);
    let (size, chrome, placement) = &window;
    save("panes", *size, &pixels);

    let pixel = |[x, y]: [f32; 2]| {
        let index = (y as usize * size[0] as usize + x as usize) * 4;
        &pixels[index..index + 3]
    };
    let active = chrome.workspaces[0].rect;
    assert_eq!(
        pixel([active.x + 8.0, active.y + 2.0]),
        [0x1d, 0x25, 0x2f],
        "picked out"
    );
    let hovered = chrome.workspaces[1].rect;
    assert_eq!(
        pixel([hovered.x + 8.0, hovered.y + 2.0]),
        [0x17, 0x1e, 0x27],
        "lit up"
    );
    assert_eq!(pixel([1.0, 1.0]), [0x0a, 0x0e, 0x13], "the window");
    let left_card = placement.panes[0].1.rect;
    let right_card = placement.panes[1].1.rect;
    // The focused card's edge is in the accent colour, for normal mode.
    assert_eq!(
        pixel([right_card.x + 0.5, right_card.y + right_card.height / 2.0]),
        [0xf8, 0xce, 0x57]
    );
    // Between the cards, only the window shows: the long line stopped.
    let between = left_card.x + left_card.width + 2.0;
    let spilled = (0..fonts.cell.height)
        .any(|y| pixel([between, placement.panes[0].1.origin[1] + y as f32]) != [0x0a, 0x0e, 0x13]);
    assert!(!spilled, "the cut-off line spilled out of its card");
}

#[test]
fn the_palette_is_drawn_over_the_cards() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let window = two_pane_window(&mut fonts, Status::Typing(":|Terminal▏".into()));
    let left = screen(20, 4, "left\r\n$ ");
    let right = screen(20, 4, "right\r\n$ ");
    let pixels = draw_window(&gpu, &mut fonts, &window, [&left, &right]);
    save("palette", window.0, &pixels);
    assert!(window.1.palette.is_some());
}
