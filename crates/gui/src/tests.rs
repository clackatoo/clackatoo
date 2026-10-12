//! Rendering a known screen offscreen, to check what the renderer draws.

use clackatoo_protocol::{Size, View};
use clackatoo_term::{Config, Terminal};

use clackatoo_protocol::{Axis, Content, Layout, Pane, PaneId, TabId, TerminalId, WorkspaceId};

use crate::chrome::{self, Chrome, Contents, Geometry, Hit, Metrics, ModeName, Status};
use crate::desktop::ButtonLayout;
use crate::fonts::{FontOptions, Fonts, Variant};
use crate::layout::Placement;
use crate::render::{CursorStyle, Highlight, HighlightKind, Picture, Renderer, Scene, SceneCursor};
use crate::screen::Screen;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The GPU device every test shares, or `None` on machines without one.
/// It's made once: tests making and dropping devices of their own at the
/// same time sometimes hung NVIDIA's Vulkan driver.
pub(crate) fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
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
        glyphs: &[],
        over_glyphs: &[],
        transparent: false,
    };
    Some((size, draw(&gpu, size, &picture, &mut fonts)))
}

/// `output` played into a terminal of `cols` × `rows`, as the window shows
/// it.
fn screen(cols: u16, rows: u16, output: &str) -> Screen {
    let mut terminal = Terminal::new(Size::new(cols, rows).unwrap(), &Config::default());
    terminal.advance(output.as_bytes());
    Screen::new(&View::capture(&terminal)).expect("a terminal's view is a grid")
}

pub(crate) fn fonts(scale: f64) -> Fonts {
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
        \x1b[41m red bg \x1b[0m 字符 e\u{301} 😀 👩\u{200D}💻 ❤\u{FE0F} #\u{FE0F}\u{20E3} 🇬🇧 👍🏽 ok\r\n\
        box: ┌─┐ │ │ └─┘  arrows: → ← ↑ ↓\r\n$ ";
    let Some((size, pixels)) = render(56, 6, output, 2.0, (false, &[])) else {
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
        glyphs: &[],
        over_glyphs: &[],
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

#[test]
fn joined_emoji_are_shaped_into_one_glyph() {
    let mut fonts = fonts(1.0);
    let Some((emoji, woman)) = fonts.glyph('👩', Variant::Regular) else {
        eprintln!("no emoji font; skipping");
        return;
    };
    let shaped = |fonts: &mut Fonts, text| {
        let glyphs = fonts.shape(text, Variant::Regular).expect(text);
        glyphs
            .iter()
            .map(|glyph| (glyph.face, glyph.glyph))
            .collect::<Vec<_>>()
    };
    let joined = shaped(&mut fonts, "👩\u{200D}💻");
    assert_eq!(joined.len(), 1, "{joined:?}");
    assert_eq!(joined[0].0, emoji);
    assert_ne!(joined[0].1, woman);
    // A keycap, through its emoji presentation selector.
    assert_eq!(shaped(&mut fonts, "#\u{FE0F}\u{20E3}").len(), 1);
    // ❤ is in text fonts too, but the selector asks for the emoji.
    assert_eq!(shaped(&mut fonts, "❤\u{FE0F}")[0].0, emoji);
}

/// A chat, as a pane shows it: messages, Markdown, tool calls, and the
/// box a message is typed in, or the agent's question.
fn chat_node(question: bool) -> clackatoo_protocol::ChatNode {
    use clackatoo_protocol::{
        ChatNode, ChatState, Choice, ChoiceKind, Diff, Item, Location, Outcome, Question, Tool,
        ToolContent, ToolKind, ToolStatus, Turn, TurnEnd,
    };
    let tool = |title: &str, status| {
        Item::Tool(Tool {
            id: title.into(),
            title: title.into(),
            status,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
        })
    };
    let build = Item::Tool(Tool {
        id: "build".into(),
        title: "cargo build".into(),
        status: ToolStatus::Failed,
        kind: ToolKind::Execute,
        content: vec![ToolContent::Text(format!(
            "```console\n   Compiling openssl-sys v0.9.103\n{}error: could not find system \
             library 'openssl' required by the 'openssl-sys' crate\nerror: could not compile \
             `openssl-sys` (build script) due to 1 previous error\n```",
            "warning: openssl-sys@0.9.103: Could not find directory of OpenSSL installation\n"
                .repeat(10)
        ))],
        locations: Vec::new(),
    });
    let edit = Item::Tool(Tool {
        id: "edit".into(),
        title: "Edit Cargo.toml".into(),
        status: ToolStatus::Running,
        kind: ToolKind::Edit,
        content: vec![ToolContent::Diff(Diff {
            path: "/src/app/Cargo.toml".into(),
            old: Some("[package]\nedition = \"2024\"\n\n[dependencies]\nopenssl = \"0.10\"\n".into()),
            new: "[package]\nedition = \"2024\"\n\n[dependencies]\nopenssl = { version = \"0.10\", features = [\"vendored\"] }\n"
                .into(),
        })],
        locations: vec![Location {
            path: "/src/app/Cargo.toml".into(),
            line: Some(98),
        }],
    });
    ChatNode {
        turns: vec![
            Turn {
                prompt: "Why is the build failing?".into(),
                sent: 0,
                items: vec![
                    tool("Read Cargo.toml", ToolStatus::Done),
                    build,
                    Item::Words(
                        "The linker can't find **libssl**. Your `Cargo.toml` asks for \
                         `openssl` 0.10 without the `vendored` feature, so it looks for the \
                         system's copy:\n\n```toml\n[dependencies]\nopenssl = \"0.10\"\n```\n\n\
                         Two ways out:\n\n- install `libssl-dev`, or\n- turn on `vendored`.\n"
                            .into(),
                    ),
                ],
                end: Some(TurnEnd {
                    at: 1,
                    outcome: Outcome::Done,
                }),
            },
            Turn {
                prompt: "Turn on vendored and try again".into(),
                sent: 2,
                items: vec![edit],
                end: None,
            },
        ],
        state: ChatState::Answering,
        question: question.then(|| Question {
            title: "run cargo build --release".into(),
            choices: vec![
                Choice {
                    id: "a".into(),
                    name: "Allow".into(),
                    kind: ChoiceKind::AllowOnce,
                },
                Choice {
                    id: "b".into(),
                    name: "Always allow".into(),
                    kind: ChoiceKind::AllowAlways,
                },
                Choice {
                    id: "c".into(),
                    name: "Reject".into(),
                    kind: ChoiceKind::RejectOnce,
                },
            ],
        }),
        notice: None,
        settings: claude_settings(),
        usage: Some(clackatoo_protocol::Usage {
            used: 38_400,
            size: 200_000,
        }),
    }
}

/// Settings as Claude Code's adapter offers them.
fn claude_settings() -> Vec<clackatoo_protocol::Setting> {
    use clackatoo_protocol::{Setting, SettingChoice, SettingKind};
    let choice = |value: &str, name: &str, description: Option<&str>| SettingChoice {
        value: value.into(),
        name: name.into(),
        description: description.map(str::to_owned),
    };
    vec![
        Setting {
            id: "mode".into(),
            name: "Mode".into(),
            kind: SettingKind::Mode,
            value: "auto".into(),
            choices: vec![
                choice(
                    "default",
                    "Manual",
                    Some("Always ask before making changes"),
                ),
                choice(
                    "acceptEdits",
                    "Accept edits",
                    Some("Automatically accept all file edits"),
                ),
                choice("plan", "Plan", Some("Create a plan before making changes")),
                choice("auto", "Auto", Some("Claude handles permission decisions")),
            ],
        },
        Setting {
            id: "model".into(),
            name: "Model".into(),
            kind: SettingKind::Model,
            value: "sonnet".into(),
            choices: vec![
                choice("default", "Default (recommended)", Some("Opus 5.5")),
                choice(
                    "opus",
                    "Opus 5.5",
                    Some("For complex work and everyday tasks"),
                ),
                choice("fable", "Fable 5.1", Some("For your toughest challenges")),
                choice(
                    "sonnet",
                    "Sonnet 5.5",
                    Some("Most efficient for simpler tasks"),
                ),
                choice("haiku", "Haiku 5.5", Some("Fastest for quick answers")),
                choice(
                    "claude-haiku-4-5-20251001",
                    "Haiku 4.5",
                    Some("Fastest for quick answers"),
                ),
                choice(
                    "claude-sonnet-5",
                    "Sonnet 5",
                    Some("Efficient for routine tasks"),
                ),
                choice(
                    "claude-opus-5",
                    "Opus 5",
                    Some("Best for everyday, complex tasks"),
                ),
                choice("claude-fable-5", "Fable 5", Some("Most capable")),
                choice(
                    "claude-opus-4-8",
                    "Opus 4.8",
                    Some("Best for everyday, complex tasks"),
                ),
                choice(
                    "claude-opus-4-7",
                    "Opus 4.7",
                    Some("Best for everyday, complex tasks"),
                ),
                choice(
                    "claude-sonnet-4-6",
                    "Sonnet 4.6",
                    Some("Efficient for routine tasks"),
                ),
            ],
        },
        Setting {
            id: "effort".into(),
            name: "Effort".into(),
            kind: SettingKind::Effort,
            value: "high".into(),
            choices: vec![
                choice("default", "Default", None),
                choice("low", "Low", None),
                choice("medium", "Medium", None),
                choice("high", "High", None),
                choice("xhigh", "Xhigh", None),
                choice("max", "Max", None),
            ],
        },
    ]
}

/// Renders a chat pane `size` pixels, and saves it as `name`.
fn render_chat(
    name: &str,
    node: clackatoo_protocol::ChatNode,
    typed: &str,
    scale: f64,
) -> Option<Vec<u8>> {
    render_chat_with(name, node, typed, scale, |_| {})
}

/// As [`render_chat`], doing `then` to the chat before drawing it.
fn render_chat_with(
    name: &str,
    node: clackatoo_protocol::ChatNode,
    typed: &str,
    scale: f64,
    then: impl FnOnce(&mut crate::chat::ChatView),
) -> Option<Vec<u8>> {
    render_chat_in(name, node, typed, (scale, None), then)
}

/// As [`render_chat_with`], with the interface font `ui`, if given.
fn render_chat_in(
    name: &str,
    node: clackatoo_protocol::ChatNode,
    typed: &str,
    (scale, ui): (f64, Option<&str>),
    then: impl FnOnce(&mut crate::chat::ChatView),
) -> Option<Vec<u8>> {
    let gpu = gpu()?;
    let mut fonts = fonts(scale);
    if let Some(ui) = ui {
        fonts.load_ui(Some(ui), 11.0, scale);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = [(720.0 * scale) as u32, (640.0 * scale) as u32];
    let mut chat = crate::chat::ChatView::new("chat".into(), node);
    if !typed.is_empty() {
        chat.paste(typed);
        chat.settle(&mut fonts);
    }
    then(&mut chat);
    #[allow(clippy::cast_precision_loss)]
    let paint = chat.paint(&mut fonts, [0.0; 2], [size[0] as f32, size[1] as f32], true);
    let mut fills = vec![crate::render::Fill::solid(
        #[allow(clippy::cast_precision_loss)]
        [0.0, 0.0, size[0] as f32, size[1] as f32],
        crate::render::Paint::Tile,
    )];
    fills.extend(paint.fills);
    let picture = Picture {
        scenes: &[],
        fills: &fills,
        over: &paint.over,
        texts: &[],
        glyphs: &paint.glyphs,
        over_glyphs: &paint.over_glyphs,
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save(name, size, &pixels);
    Some(pixels)
}

#[test]
fn renders_a_chat_with_its_box_and_question() {
    let Some(pixels) = render_chat("chat", chat_node(false), "Also run the tests", 1.0) else {
        return;
    };
    // Something was drawn: not all one colour.
    assert!(pixels.chunks(4).any(|pixel| pixel != &pixels[..4]));
    render_chat("chat-question", chat_node(true), "", 1.0);
    render_chat("chat-hidpi", chat_node(false), "", 2.0);
    // Noto Sans has no ▾: it comes from the fonts behind it.
    render_chat_in(
        "chat-noto",
        chat_node(false),
        "",
        (1.0, Some("Noto Sans")),
        |_| {},
    );
    render_chat_with("chat-menu", chat_node(false), "", 1.0, |chat| {
        use clackatoo_protocol::SettingKind;
        chat.open_menu(&[SettingKind::Model, SettingKind::Effort]);
    });
    // A command's card, folded to its output's last lines, and open.
    let mut first = chat_node(false);
    first.turns.truncate(1);
    first.turns[0].items.truncate(2);
    render_chat("chat-cards", first.clone(), "", 1.0);
    render_chat_with("chat-cards-open", first, "", 1.0, |chat| {
        chat.open_card("build");
    });
    // A pinned version: its family's versions are shown.
    let mut pinned = chat_node(false);
    pinned.settings[1].value = "claude-opus-4-8".into();
    render_chat_with("chat-menu-versions", pinned, "", 1.0, |chat| {
        use clackatoo_protocol::SettingKind;
        chat.open_menu(&[SettingKind::Model, SettingKind::Effort]);
    });
}

#[test]
fn a_chat_pane_sends_what_is_typed_and_esc_leaves_insert_mode() {
    use clackatoo_protocol::{
        ChatAction, ChatNode, ChatState, ClientMessage, Node, Request, ViewUpdate,
    };
    use winit::keyboard::{Key, KeyLocation, NamedKey};

    use crate::input::{KeyPress, Modifiers};
    use crate::pane::{Mode, Pane};

    let press = |pane: &mut Pane, key: Key, text: Option<&str>| {
        pane.key(&KeyPress {
            key: &key,
            text,
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        })
    };
    let mut pane = Pane::new(TerminalId(3));
    pane.view_changed(&ViewUpdate::Snapshot(View {
        title: "agent".into(),
        root: Node::Chat(ChatNode {
            state: ChatState::Ready,
            ..ChatNode::default()
        }),
    }));
    assert!(pane.is_chat());
    for ch in ["h", "i"] {
        press(&mut pane, Key::Character(ch.into()), Some(ch));
    }
    press(&mut pane, Key::Named(NamedKey::Enter), Some("\r"));
    // Nothing goes until the window settles it, with the fonts.
    assert!(pane.take_outbox().is_empty());
    assert!(pane.settle_chat(&mut fonts(1.0)));
    let sent = pane.take_outbox();
    assert!(
        sent.contains(&ClientMessage::Request(Request::Chat {
            terminal: TerminalId(3),
            action: ChatAction::Send("hi".into()),
        })),
        "{sent:?}"
    );
    press(&mut pane, Key::Named(NamedKey::Escape), None);
    assert_eq!(pane.mode, Mode::Normal);
    press(&mut pane, Key::Character("i".into()), Some("i"));
    assert_eq!(pane.mode, Mode::Insert);
}

#[test]
fn renders_an_editor_pane() {
    use clackatoo_protocol::{BufferId, BufferSnapshot, BufferUpdate, Kind, Span};

    use crate::editor::{BufferCopy, Editor};

    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut copy = BufferCopy::new(BufferId(1));
    copy.update(BufferUpdate::Snapshot(BufferSnapshot {
        path: "/src/main.rs".into(),
        text: "fn main() {\n\tprintln!(\"hello, 世界\");\n}\n".into(),
        version: 0,
        modified: false,
    }));
    // As tree-sitter-rust's queries colour it.
    let span = |start, end, kind| Span { start, end, kind };
    copy.update(BufferUpdate::Highlights {
        version: 0,
        start: 0,
        end: 42,
        spans: vec![
            span(0, 2, Kind::Keyword),
            span(3, 7, Kind::Function),
            span(13, 21, Kind::Macro),
            span(22, 37, Kind::String),
        ],
    });
    // A language server's error, under "hello".
    copy.update(BufferUpdate::Diagnostics {
        version: 0,
        diagnostics: vec![clackatoo_protocol::Diagnostic {
            start: 23,
            end: 28,
            severity: clackatoo_protocol::Severity::Error,
            message: "unknown greeting".into(),
            source: None,
            code: None,
        }],
    });
    let (cols, rows) = (32u16, 5u16);
    let options = crate::options::Options {
        colorcolumn: vec![20],
        ..crate::options::Options::default()
    };
    let mut editor = Editor::new(BufferId(1), &options);
    editor.resize(Size::new(cols, rows).unwrap());
    editor.settle(&copy);
    let (lines, cursor) = editor.scene();
    let mut fonts = fonts(2.0);
    let padding = 4;
    let size = [
        u32::from(cols) * fonts.cell.width + 2 * padding,
        u32::from(rows) * fonts.cell.height + 2 * padding,
    ];
    #[allow(clippy::cast_precision_loss)]
    let scene = Scene {
        origin: [padding as f32; 2],
        size: [
            (size[0] - 2 * padding) as f32,
            (size[1] - 2 * padding) as f32,
        ],
        rows: &lines,
        cursor,
        normal_mode: true,
        highlights: &editor.highlights(),
    };
    let picture = Picture {
        scenes: &[scene],
        fills: &[],
        over: &[],
        texts: &[],
        glyphs: &[],
        over_glyphs: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("editor", size, &pixels);
    // Right of the line numbers, four cells wide.
    assert_eq!(lines[1].text(), "  1     println!(\"hello, 世界\");");
    let fg = |row: &clackatoo_term::Row, col: usize| row.style(row.cells()[col]).fg;
    assert_eq!(fg(lines[0], 4), crate::colors::syntax(Kind::Keyword).fg);
    assert_eq!(fg(lines[1], 8), crate::colors::syntax(Kind::Macro).fg);
    assert_eq!(lines[3].text(), "~", "past the end");
    let background = &pixels[..4];
    assert!(pixels.chunks(4).any(|pixel| pixel != background));
    // The middle of the cell at `line` and `col`.
    let at = |line: u32, col: u32| {
        let x = padding + col * fonts.cell.width + fonts.cell.width / 2;
        let y = padding + line * fonts.cell.height + fonts.cell.height / 2;
        let start = ((y * size[0] + x) * 4) as usize;
        &pixels[start..start + 4]
    };
    // The cursor's line is lit across the pane, and column 20 down the
    // file's lines, behind the text, but not past the end. Column 20 is
    // the 20th cell right of the line numbers.
    let column = 4 + 19;
    assert_ne!(at(0, 25), background, "the cursor's line");
    assert_ne!(at(2, column), background, "the column");
    assert_eq!(at(2, column + 2), background);
    assert_eq!(at(3, column), background, "no column past the end");
}

/// How long building and sending a frame takes, on the CPU, for a screen
/// of plain text and one of emoji, joined and not. A measurement, not a
/// check: run it by hand with `cargo test -p clackatoo-gui --release
/// frame_times -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn frame_times() {
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut ascii = String::new();
    for line in 0..50 {
        ascii.extend(
            (0..160).map(|col| char::from(b'!' + u8::try_from((line * 7 + col) % 94).unwrap())),
        );
        ascii.push_str("\r\n");
    }
    let emoji = [
        "😀",
        "👩\u{200D}💻",
        "🇬🇧",
        "👍🏽",
        "1\u{FE0F}\u{20E3}",
        "e\u{301}",
        "❤\u{FE0F}",
        "🇯🇵",
        "👩🏽\u{200D}💻",
        "a",
    ];
    let mut emoji_screen = String::new();
    for line in 0..50 {
        emoji_screen.extend((0..32).map(|col| emoji[(line + col) % emoji.len()]));
        emoji_screen.push_str("\r\n");
    }
    for (name, output) in [("ascii", ascii), ("emoji", emoji_screen)] {
        let screen = screen(160, 50, &output);
        let mut fonts = fonts(2.0);
        let size = [160 * fonts.cell.width, 50 * fonts.cell.height];
        let rows: Vec<_> = screen.rows.iter().collect();
        #[allow(clippy::cast_precision_loss)]
        let scene = Scene {
            origin: [0.0; 2],
            size: [size[0] as f32, size[1] as f32],
            rows: &rows,
            cursor: None,
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
            transparent: false,
        };
        let (device, queue) = &gpu;
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut renderer = Renderer::new(device, FORMAT);
        let mut times = Vec::new();
        for frame in 0..600 {
            let started = std::time::Instant::now();
            renderer.render((device, queue), &view, size, &picture, &mut fonts);
            // The first frames rasterise glyphs, which later ones reuse.
            if frame >= 100 {
                times.push(started.elapsed().as_secs_f64() * 1e6);
            }
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("poll");
        }
        report(&format!("{name}, frame"), times);
        // Building the instances alone, the part text drawing changes.
        let times = (0..2000)
            .map(|_| {
                let started = std::time::Instant::now();
                renderer.build(&picture, &mut fonts);
                started.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        report(&format!("{name}, build"), times);
    }
}

/// Prints the median, mean and least of `times`, in microseconds.
fn report(name: &str, mut times: Vec<f64>) {
    times.sort_by(f64::total_cmp);
    #[allow(clippy::cast_precision_loss)]
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    eprintln!(
        "{name}: median {:.1} µs, mean {mean:.1} µs, min {:.1} µs",
        times[times.len() / 2],
        times[0],
    );
}
