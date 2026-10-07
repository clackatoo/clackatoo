//! Rendering a known screen offscreen, to check what the renderer draws.

use clackatoo_protocol::{Size, View};
use clackatoo_term::{Config, Terminal};

use crate::fonts::{FontOptions, Fonts};
use crate::render::{
    Bar, BarItem, CursorStyle, Highlight, HighlightKind, Picture, Renderer, Scene, SceneCursor,
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
    (normal_mode, label, highlights): (bool, Option<&str>, &[Highlight]),
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
        label,
        highlights,
    };
    let picture = Picture {
        scenes: &[scene],
        bar: None,
        dividers: &[],
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
    Fonts::load(&options, scale).expect("a monospace font")
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
    let Some((size, pixels)) = render(48, 6, output, 2.0, (false, None, &[])) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    save("text", size, &pixels);
    // Something other than the background was drawn.
    let background = &pixels[..4];
    assert!(pixels.chunks(4).any(|pixel| pixel != background));
}

#[test]
fn box_drawing_joins_across_rows_and_normal_mode_is_framed() {
    let output = "┌──┬──┐ ╭──╮ ┏━━┓ ╔══╦═╗\r\n\
        │ab│cd│ │  │ ┃  ┃ ║  ║ ║\r\n\
        ├──┼──┤ ╰──╯ ┗━━┛ ╠══╬═╣\r\n\
        └──┴──┘ ▁▂▃▄▅▆▇█ ░▒▓ ▌▐ ▖▗▘▝ ╚══╩═╝";
    let Some((size, pixels)) = render(40, 5, output, 2.0, (true, Some("NORMAL  ↑12"), &[]))
    else {
        eprintln!("no GPU available; skipping");
        return;
    };
    save("boxes", size, &pixels);
    // The frame is drawn in the accent colour, just outside the grid's top
    // left corner.
    let width = size[0] as usize;
    let framed = (0..4).any(|y| {
        (0..4).any(|x| {
            let index = (y * width + x) * 4;
            pixels[index..index + 3] == [0xf8, 0xce, 0x57]
        })
    });
    assert!(framed);
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
    let Some((size, pixels)) = render(30, 3, output, 2.0, (true, Some("VISUAL"), &highlights))
    else {
        eprintln!("no GPU available; skipping");
        return;
    };
    save("highlights", size, &pixels);
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn panes_are_drawn_side_by_side_under_the_bar_with_a_divider() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(2.0);
    let (cell_width, cell_height) = (fonts.cell.width, fonts.cell.height);
    // Two panes of 20 x 4, a divider's column between them, and the bar's
    // row above, with a padding's space below it.
    let padding = 8;
    let size = [41 * cell_width + 2 * padding, 5 * cell_height + 3 * padding];
    // Each terminal is wider than its pane, as before a resize lands: the
    // left one's long line must stop at the divider.
    let left = screen(30, 4, "left pane, with a line too long for it\r\n$ ");
    let right = screen(20, 4, "right pane\r\n$ ");
    let left_rows: Vec<_> = left.rows.iter().collect();
    let right_rows: Vec<_> = right.rows.iter().collect();
    let (padding, cell_width, cell_height) =
        (padding as f32, cell_width as f32, cell_height as f32);
    let top = 2.0 * padding + cell_height;
    let pane_size = [20.0 * cell_width, 4.0 * cell_height];
    let scene = |origin, rows, normal_mode| Scene {
        origin,
        size: pane_size,
        rows,
        cursor: None,
        normal_mode,
        label: None,
        highlights: &[],
    };
    let scenes = [
        scene([padding, top], &left_rows, false),
        scene([padding + 21.0 * cell_width, top], &right_rows, true),
    ];
    let divider_left = padding + 20.5 * cell_width - 1.0;
    let item = |text: &str, active| BarItem {
        text: text.into(),
        active,
    };
    let picture = Picture {
        scenes: &scenes,
        bar: Some(Bar {
            origin: [padding; 2],
            width: 41.0 * cell_width,
            left: &[item("clackatoo", true), item("peios", false)],
            right: &[item("1 zsh", true), item("2 htop", false)],
        }),
        dividers: &[[divider_left, top, 2.0, pane_size[1]]],
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("panes", size, &pixels);

    let pixel = |x: f32, y: f32| {
        let index = (y as usize * size[0] as usize + x as usize) * 4;
        &pixels[index..index + 3]
    };
    // The active workspace is picked out in the accent colour.
    assert_eq!(pixel(padding + 1.0, padding + 1.0), [0xf8, 0xce, 0x57]);
    // The divider is drawn, and the left pane's long line stops before it.
    assert_eq!(pixel(divider_left + 1.0, top + 1.0), [0x3c, 0x40, 0x44]);
    let background = pixel(0.0, size[1] as f32 - 1.0).to_vec();
    let text_beside_divider = (0..cell_height as usize).any(|y| {
        pixel(divider_left - cell_width / 2.0 + 2.0, top + y as f32) != background.as_slice()
    });
    assert!(
        !text_beside_divider,
        "the cut-off line spilled into the divider's cell"
    );
}
