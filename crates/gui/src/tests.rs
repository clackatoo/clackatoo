//! Rendering a known screen offscreen, to check what the renderer draws.

use clackatoo_protocol::{Size, View};
use clackatoo_term::{Config, Terminal};

use clackatoo_protocol::{Axis, Content, Layout, Pane, PaneId, TabId, TerminalId, WorkspaceId};

use crate::chrome::{
    self, Bar, Chrome, Contents, Geometry, Hit, Metrics, ModeName, NoteKind, Scrolls, Status,
    WorkspaceName,
};
use crate::desktop::ButtonLayout;
use crate::fonts::{FontOptions, Fonts, Variant};
use crate::layout::Placement;
use crate::render::{CursorStyle, Highlight, HighlightKind, Picture, Renderer, Scene, SceneCursor};
use crate::screen::Screen;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The GPU device every test shares, or `None` on machines without one:
/// the one windows drawn offscreen share. It's made once: tests making and
/// dropping devices of their own at the same time sometimes hung NVIDIA's
/// Vulkan driver.
pub(crate) fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    crate::app::look::shared_device()
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
    let screen = screen(cols, rows, output);
    let rows: Vec<_> = screen.rows.iter().collect();
    let cursor = screen.state.cursor;
    let cursor = SceneCursor {
        line: usize::from(cursor.line),
        col: usize::from(cursor.col),
        style: CursorStyle::Block,
    };
    render_rows(cols, &rows, Some(cursor), scale, (normal_mode, highlights))
}

/// Renders `rows`, `cols` wide, with `cursor`, and returns the image's
/// size and RGBA pixels.
fn render_rows(
    cols: u16,
    rows: &[&clackatoo_term::Row],
    cursor: Option<SceneCursor>,
    scale: f64,
    (normal_mode, highlights): (bool, &[Highlight]),
) -> Option<([u32; 2], Vec<u8>)> {
    let gpu = gpu()?;
    let mut fonts = fonts(scale);
    let padding = 4;
    let height = u32::try_from(rows.len()).unwrap();
    let size = [
        u32::from(cols) * fonts.cell.width + 2 * padding,
        height * fonts.cell.height + 2 * padding,
    ];
    #[allow(clippy::cast_precision_loss)]
    let scene = Scene {
        scroll: None,
        origin: [padding as f32; 2],
        size: [
            (size[0] - 2 * padding) as f32,
            (size[1] - 2 * padding) as f32,
        ],
        rows,
        cursor,
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
        images: &[],
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
    gpu: &(wgpu::Device, wgpu::Queue),
    size: [u32; 2],
    picture: &Picture<'_>,
    fonts: &mut Fonts,
) -> Vec<u8> {
    let mut renderer = Renderer::new(&gpu.0, FORMAT);
    draw_with(gpu, &mut renderer, size, picture, fonts)
}

/// Draws `picture` with `renderer`, as it is from what it drew before, in
/// an image of `size`, and returns its RGBA pixels.
fn draw_with(
    (device, queue): &(wgpu::Device, wgpu::Queue),
    renderer: &mut Renderer,
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
    renderer.render((device, queue), &view, size, picture, fonts);
    crate::render::read_back((device, queue), &texture)
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

#[test]
fn the_link_under_the_mouse_is_underlined_and_nothing_else_changes() {
    let output = "see https://x.example now";
    let link = [Highlight {
        line: 0,
        cols: 4..21,
        kind: HighlightKind::Link,
    }];
    let (Some((size, plain)), Some((_, underlined))) = (
        render(30, 2, output, 1.0, (false, &[])),
        render(30, 2, output, 1.0, (false, &link)),
    ) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let cell = fonts(1.0).cell;
    let changed: Vec<[u32; 2]> = (0..size[1])
        .flat_map(|y| (0..size[0]).map(move |x| [x, y]))
        .filter(|&[x, y]| {
            let at = usize::try_from((y * size[0] + x) * 4).unwrap();
            plain[at..at + 4] != underlined[at..at + 4]
        })
        .collect();
    assert!(!changed.is_empty(), "nothing was underlined");
    let padding = 4;
    let columns = padding + 4 * cell.width..padding + 21 * cell.width;
    let lower_half = padding + cell.height / 2..padding + cell.height;
    for [x, y] in changed {
        assert!(columns.contains(&x) && lower_half.contains(&y), "{x}, {y}");
    }
    save("link", size, &underlined);
}

#[test]
fn guesses_at_the_echo_of_what_is_typed_are_drawn_underlined() {
    use crate::input::{KeyPress, Modifiers};
    use crate::options::{Options, PredictEcho};
    use crate::pane::Pane;
    use clackatoo_protocol::{TerminalId, ViewUpdate};
    use winit::keyboard::{Key, KeyLocation};

    let mut terminal = Terminal::new(Size::new(20, 2).unwrap(), &Config::default());
    // A shell that has run a command, marked, now at its prompt.
    terminal.advance(b"\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    let mut pane = Pane::new(TerminalId(1));
    pane.set_options(&Options {
        predictecho: PredictEcho::Always,
        ..Options::default()
    });
    pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
    let draw_pane = |pane: &Pane| {
        let (rows, cursor) = pane.scene(2);
        render_rows(20, &rows, cursor, 1.0, (false, &[]))
    };
    let Some((size, plain)) = draw_pane(&pane) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    for ch in ["l", "s"] {
        pane.key(&KeyPress {
            key: &Key::Character(ch.into()),
            text: Some(ch),
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        });
    }
    let (_, guessed) = draw_pane(&pane).unwrap();
    save("predicted", size, &guessed);

    // Only the two cells typed into, and the cursor moving off the first,
    // changed: their glyphs and their underlines.
    let cell = fonts(1.0).cell;
    let padding = 4;
    let changed: Vec<[u32; 2]> = (0..size[1])
        .flat_map(|y| (0..size[0]).map(move |x| [x, y]))
        .filter(|&[x, y]| {
            let at = usize::try_from((y * size[0] + x) * 4).unwrap();
            plain[at..at + 4] != guessed[at..at + 4]
        })
        .collect();
    let cells = padding + 2 * cell.width..padding + 5 * cell.width;
    for &[x, y] in &changed {
        assert!(cells.contains(&x) && y < padding + cell.height, "{x}, {y}");
    }
    let typed = padding + 2 * cell.width..padding + 4 * cell.width;
    let upper = padding..padding + cell.height / 2;
    let lower = padding + cell.height * 3 / 4..padding + cell.height;
    for half in [&upper, &lower] {
        for column in [0, 1] {
            let start = typed.start + column * cell.width;
            let columns = start..start + cell.width;
            assert!(
                changed
                    .iter()
                    .any(|[x, y]| columns.contains(x) && half.contains(y)),
                "cell {column} lacks a glyph or an underline"
            );
        }
    }
}

#[test]
fn a_review_draws_old_lines_on_red_and_new_on_green() {
    use clackatoo_protocol::{
        Made, ReviewFile, ReviewHunk, ReviewId, ReviewList, ReviewScope, TerminalId,
    };
    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut review = crate::review::Review::new(ReviewId(1), &crate::options::Options::default());
    let (cols, rows) = (48_u16, 12_u16);
    review.resize(Size::new(cols, rows).unwrap());
    review.update(ReviewList {
        chat: Some(TerminalId(3)),
        chat_name: "Claude Code".into(),
        scope: Some(ReviewScope::Turn(Some(1))),
        files: vec![ReviewFile {
            path: "/src/main.rs".into(),
            shown: "src/main.rs".into(),
            buffer: None,
            hunks: vec![ReviewHunk {
                line: 2,
                before: vec!["fn main() {".into()],
                base: "    println!(\"old\");\n".into(),
                text: "    println!(\"new\");\n    done();\n".into(),
                after: vec!["}".into()],
                agents: vec![1],
                by: Some(Made {
                    chat: TerminalId(3),
                    turn: 1,
                }),
                reviewed: false,
            }],
        }],
        comments: Vec::new(),
        note: None,
        chats: Vec::new(),
    });
    review.settle();
    let (shown, cursor) = review.scene();
    let highlights = review.highlights();
    let mut fonts = fonts(2.0);
    let padding = 4;
    let size = [
        u32::from(cols) * fonts.cell.width + 2 * padding,
        u32::from(rows) * fonts.cell.height + 2 * padding,
    ];
    #[allow(clippy::cast_precision_loss)]
    let scene = Scene {
        scroll: None,
        origin: [padding as f32; 2],
        size: [
            (size[0] - 2 * padding) as f32,
            (size[1] - 2 * padding) as f32,
        ],
        rows: &shown,
        cursor,
        normal_mode: true,
        highlights: &highlights,
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
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("review", size, &pixels);
    // The pixel at the right end of each row, where no text is: how the
    // row is lit.
    let lit = |kind: HighlightKind| -> [u8; 4] {
        let line = highlights
            .iter()
            .find(|highlight| highlight.kind == kind)
            .expect("lit")
            .line;
        let y = padding + u32::try_from(line).unwrap() * fonts.cell.height + fonts.cell.height / 2;
        let x = size[0] - padding - 2;
        let at = usize::try_from((y * size[0] + x) * 4).unwrap();
        pixels[at..at + 4].try_into().unwrap()
    };
    let (removed, added) = (lit(HighlightKind::Removed), lit(HighlightKind::Added));
    assert!(removed[0] > removed[1], "red: {removed:?}");
    assert!(added[1] > added[0], "green: {added:?}");
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
                WorkspaceName::plain(1, "clackatoo", 2, true),
                WorkspaceName::plain(2, "peios", 1, false),
            ],
            // The second, a chat at work, is marked so.
            tabs: vec![
                (TabId(1), "1 zsh".into(), true),
                (TabId(2), "2 Claude Code ⋯".into(), false),
            ],
            status,
            buttons: &buttons,
            bottom_shown: true,
            menu,
            alert,
            rounded: false,
            finder: None,
            scrolls: Scrolls::default(),
            messages: None,
            waiting: None,
            which_keys: None,
            background: None,
        },
        &mut |text, bold| fonts.ui_width(text, bold),
    );
    let pane = |id| {
        Layout::Pane(Pane {
            id: PaneId(id),
            content: Content::Terminal(TerminalId(id)),
            home: None,
            key: None,
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
            scroll: None,
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
    fills.extend(chrome::paint_panes(&geometry, placement, focused, None));
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
        images: &[],
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
            kind: NoteKind::Label,
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
                ("clackatoo".into(), false).into(),
                ("dotfiles".into(), true).into(),
                ("homelab".into(), false).into(),
            ],
            selected: None,
            under: None,
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
            kind: NoteKind::Label,
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
#[allow(clippy::too_many_lines, reason = "one conversation, whole")]
fn chat_node(question: bool) -> clackatoo_protocol::ChatNode {
    use clackatoo_protocol::{
        ChatNode, ChatState, Choice, ChoiceKind, Diff, Item, Location, Outcome, Question, Tool,
        ToolContent, ToolKind, ToolStatus, Turn, TurnEnd, TurnFiles, TurnFilesState,
    };
    let tool = |title: &str, status| {
        Item::Tool(Tool {
            id: title.into(),
            title: title.into(),
            status,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
            exit: None,
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
        exit: None,
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
        exit: None,
    });
    ChatNode {
        turns: vec![
            Turn {
                woke: None,
                previews: Vec::new(),
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
                    raw: None,
                    spent: None,
                }),
                files: Some(TurnFiles {
                    count: 2,
                    state: TurnFilesState::Made,
                }),
                from: None,
                switch: None,
            },
            Turn {
                woke: None,
                previews: Vec::new(),
                prompt: "Turn on vendored and try again".into(),
                sent: 2,
                items: vec![edit],
                end: None,
                files: None,
                from: None,
                switch: None,
            },
        ],
        state: ChatState::Answering,
        question: question.then(|| Question {
            id: 0,
            title: "run cargo build --release".into(),
            choices: [
                ("a", "Allow", ChoiceKind::AllowOnce),
                ("b", "Always allow", ChoiceKind::AllowAlways),
                ("c", "Reject", ChoiceKind::RejectOnce),
            ]
            .map(|(id, name, kind)| Choice {
                id: id.into(),
                name: name.into(),
                kind,
            })
            .into(),
            own: false,
            form: None,
            plan: None,
            tool: None,
        }),
        settings: claude_settings(),
        usage: Some(clackatoo_protocol::Usage {
            used: 38_400,
            size: 200_000,
            five_hour: Some(clackatoo_protocol::Limit {
                used: 37,
                resets: Some(1_760_018_400_000),
            }),
            weekly: Some(clackatoo_protocol::Limit {
                used: 12,
                resets: Some(1_760_400_000_000),
            }),
            cost: None,
        }),
        ..ChatNode::default()
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
    render_chat_in(name, node, typed, (scale, None), |chat, _| then(chat))
}

/// As [`render_chat_with`], with the interface font `ui`, if given.
fn render_chat_in(
    name: &str,
    node: clackatoo_protocol::ChatNode,
    typed: &str,
    (scale, ui): (f64, Option<&str>),
    then: impl FnOnce(&mut crate::chat::ChatView, &mut crate::fonts::Fonts),
) -> Option<Vec<u8>> {
    render_chat_wide(name, node, typed, (scale, ui, 720.0), then)
}

/// As [`render_chat_in`], `width` wide before scaling.
fn render_chat_wide(
    name: &str,
    node: clackatoo_protocol::ChatNode,
    typed: &str,
    (scale, ui, width): (f64, Option<&str>, f64),
    then: impl FnOnce(&mut crate::chat::ChatView, &mut crate::fonts::Fonts),
) -> Option<Vec<u8>> {
    render_chat_sized(name, node, typed, (scale, ui, [width, 640.0]), then)
}

/// As `render_chat_wide`, with an explicit viewport height for larger modals.
fn render_chat_sized(
    name: &str,
    node: clackatoo_protocol::ChatNode,
    typed: &str,
    (scale, ui, [width, height]): (f64, Option<&str>, [f64; 2]),
    then: impl FnOnce(&mut crate::chat::ChatView, &mut crate::fonts::Fonts),
) -> Option<Vec<u8>> {
    let gpu = gpu()?;
    let mut fonts = fonts(scale);
    if let Some(ui) = ui {
        fonts.load_ui(Some(ui), 11.0, scale);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let size = [(width * scale) as u32, (height * scale) as u32];
    let mut chat = crate::chat::ChatView::new("chat".into(), node);
    // The turn going has been going 12 seconds.
    chat.set_clock(12_002);
    if !typed.is_empty() {
        chat.paste(typed);
        chat.settle(&mut fonts);
    }
    then(&mut chat, &mut fonts);
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
        images: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save(name, size, &pixels);
    Some(pixels)
}

#[test]
fn renders_wrapped_user_identifiers_without_an_oversized_bubble() {
    let prompt = format!(
        "Long identifier:\n{}\nStill visible.",
        "abcdefgh".repeat(24)
    );
    for width in [720.0, 420.0] {
        let node = crate::chat::fixtures::talk(&[(&prompt, "The message stays in its column.")]);
        render_chat_wide(
            &format!("chat-long-word-{width}"),
            node,
            "",
            (1.0, None, width),
            |_, _| {},
        );
    }
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
        |_, _| {},
    );
    render_chat_with("chat-menu", chat_node(false), "", 1.0, |chat| {
        use clackatoo_protocol::SettingKind;
        chat.open_menu(&[SettingKind::Model, SettingKind::Effort]);
    });
    // A command's card, folded to its output's last lines, and open, in
    // the turn's work, opened.
    let mut first = chat_node(false);
    first.turns.truncate(1);
    first.turns[0].items.truncate(2);
    render_chat_with("chat-cards", first.clone(), "", 1.0, |chat| {
        chat.open_fold(0);
    });
    render_chat_with("chat-cards-open", first, "", 1.0, |chat| {
        chat.open_fold(0);
        chat.open_card("build");
    });
    // A pinned version: its family's versions are shown.
    let mut pinned = chat_node(false);
    pinned.settings[1].value = "claude-opus-4-8".into();
    render_chat_with("chat-menu-versions", pinned, "", 1.0, |chat| {
        use clackatoo_protocol::SettingKind;
        chat.open_menu(&[SettingKind::Model, SettingKind::Effort]);
    });
    // Other agents' models in the menu, under the agent's own, and a line
    // across the chat where it moved to Codex.
    render_chat_with("chat-menu-agents", moved_chat_node(), "", 1.0, |chat| {
        use clackatoo_protocol::SettingKind;
        chat.open_menu(&[SettingKind::Model, SettingKind::Effort]);
    });
    let mut fast = moved_chat_node();
    fast.settings.push(clackatoo_protocol::Setting {
        id: "fast".into(),
        name: "Fast mode".into(),
        kind: clackatoo_protocol::SettingKind::Other,
        value: "on".into(),
        choices: ["off", "on"]
            .into_iter()
            .map(|value| clackatoo_protocol::SettingChoice {
                value: value.into(),
                name: value.into(),
                description: None,
            })
            .collect(),
    });
    render_chat_with(
        "chat-menu-fast-tall",
        fast,
        &"A longer draft with several lines.\n".repeat(10),
        1.0,
        |chat| {
            chat.open_menu(&[
                clackatoo_protocol::SettingKind::Model,
                clackatoo_protocol::SettingKind::Effort,
            ]);
        },
    );
}

/// [`chat_node`], moved from Claude Code to Codex after its first turn,
/// with Gemini CLI not installed.
fn moved_chat_node() -> clackatoo_protocol::ChatNode {
    use clackatoo_protocol::{
        Outcome, Provider, ProviderModels, SettingChoice, Switch, Turn, TurnEnd,
    };
    let mut node = chat_node(false);
    node.agent = "Claude Code".into();
    let choice = |value: &str, name: &str| SettingChoice {
        value: value.into(),
        name: name.into(),
        description: None,
    };
    node.providers = vec![
        Provider {
            agent: "codex".into(),
            title: "Codex".into(),
            models: ProviderModels::Known(vec![
                choice("gpt-6-astra", "6 Astra"),
                choice("gpt-6-luna", "6 Luna"),
            ]),
        },
        Provider {
            agent: "gemini".into(),
            title: "Gemini CLI".into(),
            models: ProviderModels::NotInstalled,
        },
    ];
    node.turns.insert(
        1,
        Turn {
            woke: None,
            previews: Vec::new(),
            prompt: String::new(),
            sent: 1,
            items: Vec::new(),
            end: Some(TurnEnd {
                at: 1,
                outcome: Outcome::Done,
                raw: None,
                spent: None,
            }),
            files: None,
            from: None,
            switch: Some(Switch {
                from: "Claude Code".into(),
                to: "Codex".into(),
                model: Some("6 Luna".into()),
                resumed: false,
                notes: vec!["Mode Auto doesn't carry over: Codex uses Auto review".into()],
            }),
        },
    );
    node
}

#[test]
fn renders_a_folded_turn_and_a_diff_card() {
    // The first turn, done, folds its work under how long it took; the
    // second, going, shows its work, a change to a file among it.
    let mut node = chat_node(false);
    node.turns[0].items[2] = clackatoo_protocol::Item::Words(
        "The linker can't find **libssl**: turn on `vendored`.".into(),
    );
    let Some(folded) = render_chat("chat-folded", node.clone(), "", 1.0) else {
        return;
    };
    let opened = render_chat_with("chat-unfolded", node, "", 1.0, |chat| {
        chat.open_fold(0);
    })
    .expect("drawn");
    assert!(folded != opened, "opening the work shows it");
    // In normal mode: the cursor, text selected, and the button that
    // copies a block of code, under the mouse.
    render_chat_in(
        "chat-reading",
        chat_node(false),
        "",
        (1.0, None),
        |chat, fonts| {
            chat.reading = true;
            chat.paint(fonts, [0.0; 2], [720.0, 640.0], true);
            crate::chat::fixtures::read(chat, "G{{{{wvkkll");
            chat.paint(fonts, [0.0; 2], [720.0, 640.0], true);
            chat.hover_first_code();
        },
    );
}

#[test]
fn a_chat_pane_sends_what_is_typed_and_esc_leaves_insert_mode() {
    use clackatoo_protocol::{
        ChatAction, ChatNode, ChatState, ClientMessage, Node, Request, ViewUpdate,
    };
    use winit::keyboard::{Key, KeyLocation, NamedKey};

    use crate::chat::ChatPane;
    use crate::input::{KeyPress, Modifiers};
    use crate::vim::Mode;

    let press = |pane: &mut ChatPane, key: Key, text: Option<&str>| {
        pane.key(&KeyPress {
            key: &key,
            text,
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        })
    };
    let mut pane = ChatPane::of_snapshot(
        TerminalId(3),
        &ViewUpdate::Snapshot(View {
            title: "agent".into(),
            cwd: None,
            root: Node::Chat(Box::new(ChatNode {
                state: ChatState::Ready,
                ..ChatNode::default()
            })),
        }),
    );
    for ch in ["h", "i"] {
        press(&mut pane, Key::Character(ch.into()), Some(ch));
    }
    press(&mut pane, Key::Named(NamedKey::Enter), Some("\r"));
    // Nothing goes until the window settles it, with the fonts.
    assert!(pane.take_outbox().is_empty());
    assert!(pane.settle(&mut fonts(1.0)));
    let sent = pane.take_outbox();
    assert!(
        sent.contains(&ClientMessage::Request(Request::Chat {
            terminal: TerminalId(3),
            action: ChatAction::Send("hi".into()),
        })),
        "{sent:?}"
    );
    press(&mut pane, Key::Named(NamedKey::Escape), None);
    assert_eq!(pane.mode(), Mode::Normal);
    press(&mut pane, Key::Character("i".into()), Some("i"));
    assert_eq!(pane.mode(), Mode::Insert);
}

#[test]
fn u_in_a_chat_undoes_the_last_turn_or_says_theres_none() {
    use clackatoo_protocol::{
        ChatAction, ChatNode, ChatState, ClientMessage, Node, Outcome, Request, Turn, TurnEnd,
        TurnFiles, TurnFilesState, ViewUpdate,
    };
    use winit::keyboard::{Key, KeyLocation};

    use crate::chat::ChatPane;
    use crate::input::{KeyPress, Modifiers};

    let press = |pane: &mut ChatPane, ch: &str| {
        pane.key(&KeyPress {
            key: &Key::Character(ch.into()),
            text: Some(ch),
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        })
    };
    let chat = |files: Option<TurnFiles>| {
        ViewUpdate::Snapshot(View {
            title: "agent".into(),
            cwd: None,
            root: Node::Chat(Box::new(ChatNode {
                state: ChatState::Ready,
                turns: vec![Turn {
                    woke: None,
                    previews: Vec::new(),
                    from: None,
                    prompt: "change it".into(),
                    sent: 0,
                    items: Vec::new(),
                    end: Some(TurnEnd {
                        at: 0,
                        outcome: Outcome::Done,
                        raw: None,
                        spent: None,
                    }),
                    files,
                    switch: None,
                }],
                ..ChatNode::default()
            })),
        })
    };
    let mut pane = ChatPane::of_snapshot(TerminalId(3), &chat(None));
    pane.enter_normal_mode();
    // Nothing to undo: the bar says so, until the next key.
    assert!(press(&mut pane, "u"));
    assert_eq!(pane.label().as_deref(), Some("no turn has files to undo"));
    assert!(pane.take_outbox().is_empty());
    press(&mut pane, "j");
    assert_eq!(pane.label(), None);

    let made = TurnFiles {
        count: 1,
        state: TurnFilesState::Made,
    };
    pane.view_changed(&chat(Some(made)));
    press(&mut pane, "u");
    // It goes once the chat's settled, with the fonts.
    pane.settle(&mut fonts(1.0));
    let undo = ClientMessage::Request(Request::Chat {
        terminal: TerminalId(3),
        action: ChatAction::Undo {
            turn: 0,
            force: false,
        },
    });
    assert!(pane.take_outbox().contains(&undo));
}

#[test]
// It shows everything an editor draws.
#[allow(clippy::too_many_lines)]
fn renders_an_editor_pane() {
    use clackatoo_protocol::{BufferId, BufferSnapshot, BufferUpdate, Kind, Span};
    use clackatoo_text::Sign;

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
        applied: None,
        landed: None,
        daemon: 0,
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
    // The second line changed since git's index, and one deleted after
    // the third.
    let hunk = |start, end, base: &str| clackatoo_protocol::Hunk {
        start,
        end,
        base: base.into(),
    };
    let hunks = vec![
        hunk(12, 40, "\tprint!(\"hi\");\n"),
        hunk(42, 42, "// gone\n"),
    ];
    copy.update(BufferUpdate::Hunks { version: 0, hunks });
    let (cols, rows) = (33u16, 5u16);
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
        scroll: None,
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
        images: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("editor", size, &pixels);
    // Right of the signs, a cell wide, and the line numbers, four.
    assert_eq!(lines[1].text(), "▎  1     println!(\"hello, 世界\");");
    let fg = |row: &clackatoo_term::Row, col: usize| row.style(row.cells()[col]).fg;
    assert_eq!(fg(lines[0], 5), crate::colors::syntax(Kind::Keyword).fg);
    assert_eq!(fg(lines[1], 9), crate::colors::syntax(Kind::Macro).fg);
    assert_eq!(fg(lines[1], 0), crate::colors::sign(Sign::Changed).1.fg);
    let deleted = lines[2].text().chars().next();
    assert_eq!(deleted, Some('▁'), "lines deleted after");
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
    // The changed line's sign is a thin bar down its cell's left edge.
    let x = padding + fonts.cell.width / 8;
    let y = padding + fonts.cell.height * 3 / 2;
    let start = ((y * size[0] + x) * 4) as usize;
    assert_ne!(&pixels[start..start + 4], background, "the sign");
    assert_eq!(at(0, 0), background, "no sign");
    // The cursor's line is lit across the pane, and column 20 down the
    // file's lines, behind the text, but not past the end. Column 20 is
    // the 20th cell right of the signs and line numbers.
    let column = 5 + 19;
    assert_ne!(at(0, 25), background, "the cursor's line");
    assert_ne!(at(2, column), background, "the column");
    assert_eq!(at(2, column + 2), background);
    assert_eq!(at(3, column), background, "no column past the end");
}

#[test]
// It shows everything an editor draws on a line that wraps.
#[allow(clippy::too_many_lines)]
fn renders_an_editor_pane_with_a_wrapped_line() {
    use clackatoo_protocol::{BufferId, BufferSnapshot, BufferUpdate};
    use clackatoo_term::Underline;
    use clackatoo_text::Sign;

    use crate::editor::{BufferCopy, Editor};

    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let text = "fn main() {\n    let greeting = \"hello there, wide world\";\n}\n";
    let mut copy = BufferCopy::new(BufferId(1));
    copy.update(BufferUpdate::Snapshot(BufferSnapshot {
        path: "/src/main.rs".into(),
        text: text.into(),
        version: 0,
        modified: false,
        applied: None,
        landed: None,
        daemon: 0,
    }));
    // A language server's warning, under "world", on the row the line
    // goes on over.
    let world = text.find("world").unwrap();
    copy.update(BufferUpdate::Diagnostics {
        version: 0,
        diagnostics: vec![clackatoo_protocol::Diagnostic {
            start: world,
            end: world + 5,
            severity: clackatoo_protocol::Severity::Warning,
            message: "too big".into(),
            source: None,
            code: None,
        }],
    });
    // The second line changed since git's index.
    let hunks = vec![clackatoo_protocol::Hunk {
        start: 12,
        end: text.find('}').unwrap() - 1,
        base: "    let greeting = 1;\n".into(),
    }];
    copy.update(BufferUpdate::Hunks { version: 0, hunks });
    let (cols, rows) = (30u16, 6u16);
    let options = crate::options::Options {
        colorcolumn: vec![10],
        showbreak: "↪".into(),
        ..crate::options::Options::default()
    };
    let mut editor = Editor::new(BufferId(1), &options);
    editor.resize(Size::new(cols, rows).unwrap());
    editor.go_to(1, 0, &copy);
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
        scroll: None,
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
        images: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("editor-wrapped", size, &pixels);
    // 25 cells for text, right of the sign and the numbers. The line goes
    // on over two more rows, indented as it is, after `showbreak`, with
    // no sign or number, and what's wrong written after its last.
    let texts: Vec<String> = lines.iter().map(|row| row.text()).collect();
    assert_eq!(
        texts,
        [
            "   1 fn main() {",
            "▎  2     let greeting =",
            "         ↪\"hello there, wide",
            "         ↪world\";  ■ too big",
            "   1 }",
            "~",
        ]
    );
    let style = |row: usize, col: usize| *lines[row].style(lines[row].cells()[col]);
    assert_eq!(style(1, 0).fg, crate::colors::sign(Sign::Changed).1.fg);
    let warning = crate::colors::diagnostic(clackatoo_protocol::Severity::Warning);
    assert_eq!(style(3, 10).underline, Underline::Curly, "world");
    assert_eq!(style(3, 10).underline_color, warning);
    assert_eq!(style(2, 10).underline, Underline::None);
    assert_eq!(cursor.map(|cursor| (cursor.line, cursor.col)), Some((1, 5)));
    let background = &pixels[..4];
    // The middle of the cell at `line` and `col`.
    let at = |line: u32, col: u32| {
        let x = padding + col * fonts.cell.width + fonts.cell.width / 2;
        let y = padding + line * fonts.cell.height + fonts.cell.height / 2;
        let start = ((y * size[0] + x) * 4) as usize;
        &pixels[start..start + 4]
    };
    // The sign, a thin bar down its cell's left edge, on the first row
    // only.
    let sign = |line: u32| {
        let x = padding + fonts.cell.width / 8;
        let y = padding + line * fonts.cell.height + fonts.cell.height / 2;
        let start = ((y * size[0] + x) * 4) as usize;
        &pixels[start..start + 4]
    };
    assert_ne!(sign(1), background, "the sign");
    assert_eq!(sign(2), background, "no sign on the rows after");
    // The cursor's line is lit on all its rows, and no other.
    for line in 1..=3 {
        assert_ne!(at(line, 28), background, "the cursor's line, row {line}");
    }
    assert_eq!(at(4, 28), background);
    // Column 10 is marked on each line's first row, where it is in the
    // line, and not on the rows after, which start further along.
    let column = 5 + 9;
    assert_ne!(at(4, column), background, "the column");
    let marked: Vec<usize> = (editor.highlights().into_iter())
        .filter(|highlight| highlight.kind == HighlightKind::ColorColumn)
        .map(|highlight| highlight.line)
        .collect();
    assert_eq!(marked, [0, 1, 4]);
}

/// How long building and sending a frame takes, on the CPU, for a screen
/// of plain text and one of emoji, joined and not. A measurement, not a
/// check: run it by hand with `cargo test -p clackatoo-gui --release
/// frame_times -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
#[allow(
    clippy::too_many_lines,
    reason = "shared GPU setup and timing for static and moving frames"
)]
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
    for (name, output, moving) in [
        ("ascii", ascii.clone(), false),
        ("emoji", emoji_screen.clone(), false),
        ("ascii scrolling", ascii, true),
        ("emoji scrolling", emoji_screen, true),
    ] {
        let screen = screen(160, 50, &output);
        let mut fonts = fonts(2.0);
        let size = [160 * fonts.cell.width, 50 * fonts.cell.height];
        let rows: Vec<_> = screen
            .rows
            .iter()
            .chain(screen.rows.iter().take(usize::from(moving)))
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let scene = Scene {
            scroll: moving.then_some(crate::render::RowScroll {
                first: 0,
                end: 50,
                fraction: 0.5,
            }),
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
            images: &[],
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

#[test]
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn an_editors_completion_menu_and_signature_are_drawn_by_its_cursor() {
    use clackatoo_protocol::{
        Answer, BufferId, BufferSnapshot, BufferUpdate, ClientMessage, CompletionItem, Query,
        ServerFeatures,
    };
    use winit::keyboard::{Key, KeyLocation};

    use crate::editor::{BufferCopy, Editor, Shared};
    use crate::input::KeyPress;

    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut copy = BufferCopy::new(BufferId(1));
    let text = "fn main() {\n    let value = 1;\n    \n}\n".to_owned()
        + &"// xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n".repeat(12);
    copy.update(BufferUpdate::Snapshot(BufferSnapshot {
        path: "/a.rs".into(),
        text,
        version: 0,
        modified: false,
        applied: None,
        landed: None,
        daemon: 0,
    }));
    copy.update(BufferUpdate::Features(ServerFeatures {
        completion: Some(vec!['.']),
        signature: Some(vec!['(', ',']),
        signature_again: vec![')'],
        prepare_rename: false,
        uses: false,
        inlay_hints: false,
    }));
    let (cols, rows) = (84u16, 14u16);
    let mut editor = Editor::new(BufferId(1), &crate::options::Options::default());
    editor.resize(Size::new(cols, rows).unwrap());
    let mut shared = Shared::default();
    let mut version = 0;
    // Types `keys`, answering each edit as the daemon would, and returns
    // the queries sent.
    let mut type_keys = |editor: &mut Editor, copy: &mut BufferCopy, keys: &str| {
        for ch in keys.chars() {
            let key = Key::Character(ch.to_string().into());
            let text = ch.to_string();
            let press = KeyPress {
                key: &key,
                text: Some(&text),
                location: KeyLocation::Standard,
                code: None,
                modifiers: crate::input::Modifiers::default(),
            };
            editor.key(&press, copy, &mut shared);
        }
        editor.tick(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            copy,
        );
        let mut queries = Vec::new();
        loop {
            let messages = copy.take_outbox();
            if messages.is_empty() {
                return (queries, version);
            }
            for message in messages {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        version += 1;
                        copy.update(BufferUpdate::Acked { version, seq });
                    }
                    ClientMessage::Query { id, query, .. } => queries.push((id, query)),
                    _ => {}
                }
            }
        }
    };
    let answer = |editor: &mut Editor, copy: &mut BufferCopy, id, version, answer| {
        let mut heard = copy.update(BufferUpdate::Answer {
            id,
            version,
            answer,
        });
        editor.heard(&mut heard, true, copy);
    };
    type_keys(&mut editor, &mut copy, "jjA");
    let (queries, now) = type_keys(&mut editor, &mut copy, "add(1, va");
    let label = "fn add(a: u32, value: u32) -> u32";
    let start = label.find("value").unwrap();
    for (id, query) in queries.into_iter().rev() {
        match query {
            Query::SignatureHelp { .. } => {
                let signature = Answer::Signature {
                    label: label.into(),
                    parameter: Some((start, start + "value: u32".len())),
                    index: 0,
                    count: 2,
                };
                answer(&mut editor, &mut copy, id, now, signature);
            }
            Query::Completion(_) => {
                let at = "fn main() {\n    let value = 1;\n    add(1, ".len();
                let item = |label: &str, kind, detail: &str| CompletionItem {
                    label: label.into(),
                    kind,
                    detail: detail.into(),
                    filter: None,
                    sort: None,
                    preselect: false,
                    start: Some(at),
                    text: label.into(),
                    snippet: false,
                    more: Vec::new(),
                };
                let items = vec![
                    item("value", 6, "u32"),
                    item("validate", 3, "fn(&str) -> bool"),
                    item("Variant", 20, ""),
                    item("vec!", 15, "macro"),
                ];
                let completions = Answer::Completions {
                    items,
                    incomplete: false,
                };
                answer(&mut editor, &mut copy, id, now, completions);
            }
            _ => {}
        }
    }
    // Its documentation, resolved.
    let (queries, now) = type_keys(&mut editor, &mut copy, "");
    if let Some(&(id, _)) = queries
        .iter()
        .find(|(_, query)| matches!(query, Query::Resolve { .. }))
    {
        let resolved = Answer::Resolved {
            detail: Some("let value: u32".into()),
            documentation: Some(("The value, **counted** in `u32`s.".into(), true)),
        };
        answer(&mut editor, &mut copy, id, now, resolved);
    }
    editor.settle(&copy);
    let views = editor.panels();
    assert_eq!(
        views.len(),
        3,
        "the signature, the menu and its documentation"
    );
    let (signature, menu) = (views[0].placed, views[1].placed);
    assert!(signature.top < 2 && menu.top > 2, "{signature:?} {menu:?}");

    let mut fonts = fonts(2.0);
    let metrics = Metrics {
        cell: [fonts.cell.width as f32, fonts.cell.height as f32],
        ui_line: fonts.ui.height,
        padding: 8.0,
    };
    let padding = 4;
    let size = [
        u32::from(cols) * fonts.cell.width + 2 * padding,
        u32::from(rows) * fonts.cell.height + 2 * padding,
    ];
    let geometry = Geometry::new(size, metrics);
    let origin = [padding as f32; 2];
    let (mut over, mut texts) = (Vec::new(), Vec::new());
    for view in &views {
        chrome::paint_panel(&geometry, origin, view, &mut over, &mut texts);
    }
    let (lines, cursor) = editor.scene();
    let scene = Scene {
        scroll: None,
        origin,
        size: [
            (size[0] - 2 * padding) as f32,
            (size[1] - 2 * padding) as f32,
        ],
        rows: &lines,
        cursor,
        normal_mode: false,
        highlights: &[],
    };
    let picture = Picture {
        scenes: &[scene],
        fills: &[],
        over: &over,
        texts: &texts,
        glyphs: &[],
        over_glyphs: &[],
        images: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("editor-completion", size, &pixels);
    let at = |line: usize, col: usize| {
        let x = padding as usize + col * fonts.cell.width as usize + fonts.cell.width as usize / 2;
        let y =
            padding as usize + line * fonts.cell.height as usize + fonts.cell.height as usize / 4;
        let start = (y * size[0] as usize + x) * 4;
        [pixels[start], pixels[start + 1], pixels[start + 2]]
    };
    // The item picked is lit across the menu; the next isn't.
    let right = menu.left + menu.width;
    assert_ne!(at(menu.top, right), at(menu.top + 1, right));
}

#[test]
#[allow(clippy::cast_precision_loss)]
fn an_editors_panel_is_drawn_over_its_text() {
    use clackatoo_protocol::{Answer, BufferId, BufferSnapshot, BufferUpdate, ClientMessage};
    use winit::keyboard::{Key, KeyLocation};

    use crate::editor::{BufferCopy, Editor, Shared};
    use crate::input::KeyPress;

    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut copy = BufferCopy::new(BufferId(1));
    let text = format!(
        "fn bad() {{}}\n{}",
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxx\n".repeat(8)
    );
    copy.update(BufferUpdate::Snapshot(BufferSnapshot {
        path: "/a.rs".into(),
        text,
        version: 0,
        modified: false,
        applied: None,
        landed: None,
        daemon: 0,
    }));
    let (cols, rows) = (32u16, 9u16);
    let mut editor = Editor::new(BufferId(1), &crate::options::Options::default());
    editor.resize(Size::new(cols, rows).unwrap());
    // K, and the language server's answer.
    let key = Key::Character("K".into());
    let press = KeyPress {
        key: &key,
        text: Some("K"),
        location: KeyLocation::Standard,
        code: None,
        modifiers: crate::input::Modifiers::default(),
    };
    editor.key(&press, &mut copy, &mut Shared::default());
    let Some(ClientMessage::Query { id, .. }) = copy.take_outbox().pop() else {
        panic!("a query");
    };
    let mut heard = copy.update(BufferUpdate::Answer {
        id,
        version: 0,
        answer: Answer::Hover {
            text: "```rust\nfn bad()\n```\n\nIt's **bad**: see `good`.".into(),
            markdown: true,
            range: None,
        },
    });
    editor.heard(&mut heard, true, &mut copy);
    editor.settle(&copy);
    let view = editor.panel().expect("a panel");
    // By the cursor, right of the signs and line numbers.
    assert_eq!((view.placed.top, view.placed.left), (1, 5));

    let mut fonts = fonts(2.0);
    let metrics = Metrics {
        cell: [fonts.cell.width as f32, fonts.cell.height as f32],
        ui_line: fonts.ui.height,
        padding: 8.0,
    };
    let padding = 4;
    let size = [
        u32::from(cols) * fonts.cell.width + 2 * padding,
        u32::from(rows) * fonts.cell.height + 2 * padding,
    ];
    let geometry = Geometry::new(size, metrics);
    let origin = [padding as f32; 2];
    let (mut over, mut texts) = (Vec::new(), Vec::new());
    chrome::paint_panel(&geometry, origin, &view, &mut over, &mut texts);
    let (lines, cursor) = editor.scene();
    let scene = Scene {
        scroll: None,
        origin,
        size: [
            (size[0] - 2 * padding) as f32,
            (size[1] - 2 * padding) as f32,
        ],
        rows: &lines,
        cursor,
        normal_mode: true,
        highlights: &[],
    };
    let picture = Picture {
        scenes: &[scene],
        fills: &[],
        over: &over,
        texts: &texts,
        glyphs: &[],
        over_glyphs: &[],
        images: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("editor-panel", size, &pixels);
    let at = |line: u32, col: u32| {
        let x = padding + col * fonts.cell.width + fonts.cell.width / 2;
        let y = padding + line * fonts.cell.height + fonts.cell.height / 2;
        let start = ((y * size[0] + x) * 4) as usize;
        [pixels[start], pixels[start + 1], pixels[start + 2]]
    };
    // In the room left of its text, over an "x" of the file's, only the
    // panel's background; and the file's text beside it, not.
    let palette = [0x16, 0x18, 0x1c];
    assert_eq!(at(3, 5), palette, "the panel, over the text");
    assert_ne!(at(3, 30), palette, "the file, beside it");
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
#[allow(clippy::too_many_lines, reason = "one rendered finder fixture")]
fn the_finder_is_drawn_over_the_panes_with_a_preview() {
    use crate::finder::view::Layout as FinderLayout;

    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let mut fonts = fonts(1.0);
    let metrics = Metrics {
        cell: [fonts.cell.width as f32, fonts.cell.height as f32],
        ui_line: fonts.ui.height,
        padding: 4.0,
    };
    let [width, height] = Geometry::window_size(150, 36, metrics);
    let size = [width as u32, height as u32];
    let geometry = Geometry::new(size, metrics);
    let view = grep_finder_view(&FinderLayout::new(&geometry));
    assert!(view.preview.is_some());
    let buttons = ButtonLayout::default();
    let chrome = Chrome::new(
        geometry,
        Contents {
            workspaces: vec![WorkspaceName::plain(1, "clackatoo", 2, true)],
            tabs: vec![(TabId(1), "1 zsh".into(), true)],
            status: Status::tabs(),
            buttons: &buttons,
            bottom_shown: true,
            menu: None,
            alert: None,
            rounded: false,
            finder: Some(view),
            scrolls: Scrolls::default(),
            messages: None,
            waiting: None,
            which_keys: None,
            background: None,
        },
        &mut |text, bold| fonts.ui_width(text, bold),
    );
    // Clicks on the panel are its own, a row's or none's: never the pane's.
    let (finder, _) = chrome.finder.as_ref().expect("the finder");
    let panel = finder.panel;
    assert_eq!(
        chrome.hit([panel.x + 2.0, panel.y + 2.0], false),
        Hit::Finder(None)
    );
    let first_row = (0..200)
        .map(|y| [panel.x + 30.0, panel.y + y as f32])
        .find(|&point| finder.row_at(point, 3) == Some(0))
        .expect("a row");
    assert_eq!(chrome.hit(first_row, false), Hit::Finder(Some(0)));
    let layout = Layout::Pane(Pane {
        id: PaneId(1),
        content: Content::Terminal(TerminalId(1)),
        home: None,
        key: None,
    });
    let placement = Placement::new(&layout, &geometry.area);
    let screen = screen(150, 36, "$ ls\r\nCargo.toml  crates\r\n$ ");
    let rows: Vec<_> = screen.rows.iter().collect();
    let scenes = [Scene {
        scroll: None,
        origin: placement.panes[0].1.origin,
        size: placement.panes[0].1.cells_size(geometry.area.cell),
        rows: &rows,
        cursor: None,
        normal_mode: true,
        highlights: &[],
    }];
    let painted = chrome.paint(Hit::Nothing);
    let mut fills = vec![chrome::window_fill(&geometry, false)];
    fills.extend(painted.fills);
    fills.extend(chrome::paint_panes(&geometry, &placement, None, None));
    let mut texts = painted.texts;
    texts.extend(painted.over_texts);
    let picture = Picture {
        scenes: &scenes,
        fills: &fills,
        over: &painted.over,
        texts: &texts,
        glyphs: &[],
        over_glyphs: &[],
        images: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("finder", size, &pixels);
    // The row picked stands out from the panel, in the colour every list
    // over the panes picks in; the next row doesn't.
    let at = |x: f32, y: f32| {
        let index = (y as usize * size[0] as usize + x as usize) * 4;
        [pixels[index], pixels[index + 1], pixels[index + 2]]
    };
    let theme = crate::colors::Theme::default();
    let rgb = |rgba: crate::colors::Rgba| rgba.map(|c| (c * 255.0).round() as u8);
    let [chosen, palette] = [theme.chosen, theme.palette_background].map(rgb);
    let right = finder.list.x + finder.list.width - 6.0;
    let row_height = (1..200)
        .map(|dy| first_row[1] + dy as f32)
        .find(|&y| finder.row_at([first_row[0], y], 3) == Some(1))
        .expect("a second row")
        - first_row[1];
    let picked = at(right, first_row[1] + row_height / 2.0);
    let next = at(right, first_row[1] + row_height * 1.5);
    assert_eq!(picked[..], chosen[..3]);
    assert_eq!(next[..], palette[..3]);
}

/// What a grep's finder shows, laid out as `layout` says, with three lines
/// found and a preview of the first.
fn grep_finder_view(layout: &crate::finder::view::Layout) -> crate::finder::view::FinderView {
    use clackatoo_protocol::{Found, FoundLine};

    use crate::finder::{Finder, Kind};
    use crate::options::Case;

    let mut finder = Finder::new(Kind::Grep, "fn ma".into(), Case::default());
    finder.searching(1);
    finder.found(Found::Root {
        path: "/home/jack/projects/clackatoo".into(),
        name: "~/projects/clackatoo".into(),
    });
    let line = |path: &str, line, text: &str| FoundLine {
        path: path.into(),
        line,
        col: 0,
        text: text.into(),
        matches: vec![(0, 5)],
    };
    finder.found(Found::Lines(vec![
        line("crates/clackatoo/src/main.rs", 40, "fn main() {"),
        line("crates/daemon/examples/fake_agent.rs", 12, "fn main() {"),
        line(
            "crates/gui/src/desk/finding.rs",
            220,
            "fn make_it(&mut self) {}",
        ),
    ]));
    let main = "/home/jack/projects/clackatoo/crates/clackatoo/src/main.rs";
    finder.previewing(2, main.into(), 40);
    let lines: Vec<String> = (0..60)
        .map(|n| match n {
            40 => "fn main() {".to_owned(),
            41 => "\tlet args = Args::parse();".to_owned(),
            _ => format!("// line {n}"),
        })
        .collect();
    finder.preview_found(Found::Preview {
        first: 0,
        lines,
        spans: Vec::new(),
    });
    finder.view((layout.rows, layout.cols), layout.preview_size)
}

#[test]
// It shows everything an explorer draws.
#[allow(clippy::too_many_lines)]
fn renders_an_explorer_pane() {
    use clackatoo_protocol::{Entry, EntryKind, FilesId, FilesUpdate, GitMark};

    use crate::explorer::Explorer;
    use crate::vim::Stroke;

    let Some(gpu) = gpu() else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let (cols, rows) = (24u16, 7u16);
    let mut explorer = Explorer::new(FilesId(1), &crate::options::Options::default());
    explorer.resize(Size::new(cols, rows).unwrap());
    let entry = |name: &str, kind, ignored| Entry {
        name: name.into(),
        kind,
        ignored,
        stamp: None,
    };
    let file = EntryKind::File { executable: false };
    explorer.update(FilesUpdate::Root {
        path: "/p".into(),
        name: "~/p".into(),
    });
    explorer.update(FilesUpdate::Listed {
        dir: "/p".into(),
        entries: vec![
            entry("src", EntryKind::Dir, false),
            entry("target", EntryKind::Dir, true),
            entry("build.sh", EntryKind::File { executable: true }, false),
            entry("README.md", file, false),
        ],
        cut_short: false,
    });
    explorer.stroke(Stroke::char('l'));
    explorer.update(FilesUpdate::Listed {
        dir: "/p/src".into(),
        entries: vec![entry("main.rs", file, false)],
        cut_short: false,
    });
    explorer.update(FilesUpdate::Git(vec![(
        "/p/src/main.rs".into(),
        GitMark::Modified,
    )]));
    explorer.stroke(Stroke::char('j'));
    explorer.settle();
    let (lines, cursor) = explorer.scene();
    let mut fonts = fonts(2.0);
    let padding = 4;
    let size = [
        u32::from(cols) * fonts.cell.width + 2 * padding,
        u32::from(rows) * fonts.cell.height + 2 * padding,
    ];
    #[allow(clippy::cast_precision_loss)]
    let scene = Scene {
        scroll: None,
        origin: [padding as f32; 2],
        size: [
            (size[0] - 2 * padding) as f32,
            (size[1] - 2 * padding) as f32,
        ],
        rows: &lines,
        cursor,
        normal_mode: true,
        highlights: &explorer.highlights(),
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
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    save("explorer", size, &pixels);
    let text: Vec<String> = lines.iter().map(|row| row.text()).collect();
    assert_eq!(
        text,
        [
            "~/p",
            "    ..",
            "M ▾ src",
            "M │   main.rs",
            "  ▸ target",
            "    build.sh",
            "    README.md"
        ]
    );
    let fg = |row: &clackatoo_term::Row, col: usize| row.style(row.cells()[col]).fg;
    assert_eq!(
        fg(lines[3], 0),
        crate::colors::git_mark(GitMark::Modified).1.fg
    );
    let up = lines[1].style(lines[1].cells()[4]);
    assert_eq!(
        up.fg,
        clackatoo_protocol::Color::Indexed(4),
        "as a directory"
    );
    assert_eq!(
        up.attributes,
        clackatoo_protocol::Attributes::BOLD,
        "at a directory's full contrast"
    );
    assert_eq!(
        fg(lines[2], 4),
        clackatoo_protocol::Color::Indexed(4),
        "a directory"
    );
    assert_eq!(
        fg(lines[5], 4),
        clackatoo_protocol::Color::Indexed(2),
        "a program"
    );
    let background = &pixels[..4];
    // The middle of the cell at `line` and `col`.
    let at = |line: u32, col: u32| {
        let x = padding + col * fonts.cell.width + fonts.cell.width / 2;
        let y = padding + line * fonts.cell.height + fonts.cell.height / 2;
        let start = ((y * size[0] + x) * 4) as usize;
        &pixels[start..start + 4]
    };
    // The cursor's line is lit across the pane, and nothing past it.
    assert_ne!(at(3, 20), background, "the cursor's line");
    assert_eq!(at(6, 20), background);
    assert_ne!(at(3, 0), background, "the mark");
}

#[test]
fn renders_a_lesson_with_a_tick_by_a_step_done_and_its_code_on_a_background() {
    use clackatoo_protocol::{BufferId, BufferSnapshot, BufferUpdate};

    use crate::editor::{BufferCopy, Editor, Lit, Prose, Stretch};

    let text = "1. Done\n   Type :w now\n";
    let mut copy = BufferCopy::new(BufferId(1));
    copy.update(BufferUpdate::Snapshot(BufferSnapshot {
        path: "/tutor/lesson.txt".into(),
        text: text.into(),
        version: 0,
        modified: false,
        applied: None,
        landed: None,
        daemon: 0,
    }));
    let (cols, rows) = (20u16, 3u16);
    let options = crate::options::Options::default();
    let mut editor = Editor::new(BufferId(1), &options);
    editor.resize(Size::new(cols, rows).unwrap());
    let code = text.find(":w").unwrap();
    let bold = clackatoo_protocol::Style {
        attributes: clackatoo_protocol::Attributes::BOLD,
        ..clackatoo_protocol::Style::DEFAULT
    };
    let prose = Prose {
        styled: vec![Stretch {
            start: 0,
            end: 7,
            style: bold,
        }],
        code: vec![(code, code + 2)],
    };
    editor.show_lesson(&options, &copy, prose);
    editor.light(vec![Lit {
        lines: 0..1,
        kind: HighlightKind::LessonDone,
        until: None,
    }]);
    editor.settle(&copy);
    let (lines, _) = editor.scene();
    let Some((size, pixels)) = render_rows(cols, &lines, None, 2.0, (true, &editor.highlights()))
    else {
        eprintln!("no GPU available; skipping");
        return;
    };
    save("lesson", size, &pixels);
    assert_eq!(lines[1].text(), "     Type :w now");
    let fonts = fonts(2.0);
    let padding = 4;
    // The pixels of the cell at `line` and `col`.
    let cell = |line: u32, col: u32| -> Vec<&[u8]> {
        let (width, height) = (fonts.cell.width, fonts.cell.height);
        let mut cell = Vec::new();
        for y in padding + line * height..padding + (line + 1) * height {
            for x in padding + col * width..padding + (col + 1) * width {
                let start = ((y * size[0] + x) * 4) as usize;
                cell.push(&pixels[start..start + 4]);
            }
        }
        cell
    };
    // The tick is green.
    let green = cell(0, 0)
        .iter()
        .filter(|pixel| u32::from(pixel[1]) > u32::from(pixel[2]) + 60)
        .count();
    assert!(green > 4, "a green tick");
    // Code is on a background of its own, a cell before it isn't.
    let corner = |line, col| cell(line, col)[0].to_vec();
    let background = pixels[..4].to_vec();
    assert_ne!(corner(1, 10), background, "behind the code");
    assert_eq!(corner(1, 9), background, "before it");
}

#[test]
fn a_turn_is_undone_by_its_number_which_skips_where_the_chat_moved() {
    use clackatoo_protocol::{ChatAction, ClientMessage, Node, Request, ViewUpdate};

    use crate::chat::ChatPane;

    let mut pane = ChatPane::of_snapshot(
        TerminalId(3),
        &ViewUpdate::Snapshot(View {
            title: "agent".into(),
            cwd: None,
            root: Node::Chat(Box::new(moved_chat_node())),
        }),
    );
    // Turn 2 is the third: the second shows the move.
    pane.undo_turn(Some(1), false, false).unwrap();
    assert!(
        pane.take_outbox()
            .contains(&ClientMessage::Request(Request::Chat {
                terminal: TerminalId(3),
                action: ChatAction::Undo {
                    turn: 2,
                    force: false
                },
            }))
    );
    assert_eq!(
        pane.undo_turn(Some(2), false, false),
        Err("there's no turn 3".to_owned())
    );
}

#[test]
fn modal_images_are_drawn_above_the_backdrop_using_the_same_texture() {
    use crate::render::{Fill, Image, Paint, PlacedImage};
    let Some(gpu) = gpu() else {
        return;
    };
    let mut fonts = fonts(1.0);
    let mut renderer = Renderer::new(&gpu.0, FORMAT);
    let image = Image {
        id: 1,
        width: 5000,
        height: 1,
        rgba: [255, 0, 0, 255].repeat(5000),
    };
    let images = [
        PlacedImage {
            over: false,
            slot: 7,
            rect: [0.0, 0.0, 20.0, 20.0],
            image: &image,
            clip: None,
        },
        PlacedImage {
            over: true,
            slot: 7,
            rect: [20.0, 0.0, 20.0, 20.0],
            image: &image,
            clip: None,
        },
    ];
    let backdrop = [Fill::solid([0.0, 0.0, 40.0, 20.0], Paint::ImageBackdrop)];
    let picture = Picture {
        scenes: &[],
        fills: &[],
        over: &backdrop,
        texts: &[],
        glyphs: &[],
        over_glyphs: &[],
        images: &images,
        transparent: true,
    };
    let pixels = draw_with(&gpu, &mut renderer, [40, 20], &picture, &mut fonts);
    let pixel = |x: usize| &pixels[(10 * 40 + x) * 4..(10 * 40 + x) * 4 + 4];
    assert!(pixel(5)[0] < 80, "ordinary image is dimmed");
    assert_eq!(pixel(25), [255, 0, 0, 255], "modal image stays bright");
}

#[test]
fn a_picture_is_drawn_where_it_is_placed_and_uploaded_again_when_it_changes() {
    use crate::render::{Image, PlacedImage};
    let Some(gpu) = gpu() else {
        return;
    };
    let mut fonts = fonts(1.0);
    let mut renderer = Renderer::new(&gpu.0, FORMAT);
    let size = [40, 20];
    // A 2×1 picture: red on the left, blue on the right, stretched over
    // the left half of the image.
    let image = |id, left: [u8; 4], right: [u8; 4]| Image {
        id,
        width: 2,
        height: 1,
        rgba: [left, right].concat(),
    };
    let pixel = |pixels: &[u8], [x, y]: [usize; 2]| -> [u8; 4] {
        let at = (y * size[0] as usize + x) * 4;
        pixels[at..at + 4].try_into().unwrap()
    };
    let first = image(1, [255, 0, 0, 255], [0, 0, 255, 255]);
    let placed = [PlacedImage {
        over: false,
        clip: None,
        slot: 7,
        rect: [0.0, 0.0, 20.0, 20.0],
        image: &first,
    }];
    let mut picture = Picture {
        scenes: &[],
        fills: &[],
        over: &[],
        texts: &[],
        glyphs: &[],
        over_glyphs: &[],
        images: &placed,
        transparent: true,
    };
    let pixels = draw_with(&gpu, &mut renderer, size, &picture, &mut fonts);
    assert_eq!(pixel(&pixels, [2, 10]), [255, 0, 0, 255], "red on its left");
    assert_eq!(
        pixel(&pixels, [17, 10]),
        [0, 0, 255, 255],
        "blue on its right"
    );
    assert_eq!(pixel(&pixels, [30, 10])[3], 0, "nothing past it");
    // The next picture in the same slot replaces it.
    let second = image(2, [0, 255, 0, 255], [0, 255, 0, 255]);
    let placed = [PlacedImage {
        over: false,
        clip: None,
        slot: 7,
        rect: [0.0, 0.0, 20.0, 20.0],
        image: &second,
    }];
    picture.images = &placed;
    let pixels = draw_with(&gpu, &mut renderer, size, &picture, &mut fonts);
    assert_eq!(pixel(&pixels, [2, 10]), [0, 255, 0, 255]);
    // A scrolled picture is clipped without squeezing its UV coordinates.
    let clipped = [PlacedImage {
        over: false,
        slot: 7,
        rect: [0.0, 0.0, 20.0, 20.0],
        image: &first,
        clip: Some([10.0, 0.0, 10.0, 20.0]),
    }];
    picture.images = &clipped;
    let pixels = draw_with(&gpu, &mut renderer, size, &picture, &mut fonts);
    assert_eq!(pixel(&pixels, [2, 10])[3], 0);
    assert_eq!(pixel(&pixels, [17, 10]), [0, 0, 255, 255]);
    let translucent = image(4, [0, 255, 0, 128], [0, 255, 0, 128]);
    let placed = [PlacedImage {
        over: false,
        slot: 7,
        rect: [0.0, 0.0, 20.0, 20.0],
        image: &translucent,
        clip: None,
    }];
    picture.images = &placed;
    let pixels = draw_with(&gpu, &mut renderer, size, &picture, &mut fonts);
    assert_eq!(pixel(&pixels, [2, 10]), [0, 128, 0, 128]);
    // One wider than any window draws isn't drawn.
    let wide = Image {
        id: 3,
        width: crate::render::MAX_IMAGE_SIDE + 1,
        height: 1,
        rgba: [255, 0, 0, 255].repeat(crate::render::MAX_IMAGE_SIDE as usize + 1),
    };
    let placed = [PlacedImage {
        over: false,
        clip: None,
        slot: 8,
        rect: [0.0, 0.0, 20.0, 20.0],
        image: &wide,
    }];
    picture.images = &placed;
    let pixels = draw_with(&gpu, &mut renderer, size, &picture, &mut fonts);
    assert_eq!(pixel(&pixels, [2, 10])[3], 0, "nothing drawn");
}

#[test]
fn a_chats_normal_mode_cursor_is_drawn_solid_and_its_code_and_tables_as_they_should_be() {
    use clackatoo_protocol::{Kind, Span};

    let answer =
        "Some words.\n\n```rust\nfn main() {}\n```\n\n| name | size |\n|---|---|\n| a | 1 |";
    let node = crate::chat::fixtures::talk(&[("show me", answer)]);
    let mut cursor = None;
    let Some(pixels) = render_chat_with("chat-cursor", node, "", 1.0, |chat| {
        // The code's colours have come, and the chat's in normal mode.
        chat.paint(&mut fonts(1.0), [0.0; 2], [720.0, 640.0], true);
        let asks = chat.take_colour_asks();
        chat.colours_asked(Some(1), asks[0].clone());
        let keyword = Span {
            start: 0,
            end: 2,
            kind: Kind::Keyword,
        };
        chat.coloured(1, vec![vec![keyword]]);
        chat.reading = true;
        cursor = Some(());
    }) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    assert!(cursor.is_some());
    let width = 720;
    let at = |x: usize, y: usize| {
        let at = (y * width + x) * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    };
    let gold = |[r, g, b]: [u8; 3]| r > 0xb0 && g > 0x90 && b < 0x80;
    // Solid: somewhere in the conversation, a block of the accent colour
    // four pixels square, which a hollow box's edge isn't.
    let solid = (0..470).any(|y| {
        (0..width - 4).any(|x| (0..4).all(|dy| (0..4).all(|dx| gold(at(x + dx, y + dy)))))
    });
    assert!(solid, "the cursor is drawn solid");
    // `fn` in the keywords' colour, which plain text never is.
    let keyword = crate::colors::syntax(Kind::Keyword).fg;
    let theme = crate::colors::Theme::default();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let [r, g, b] = {
        let rgba = theme.resolve(keyword).expect("a colour of the palette");
        [0, 1, 2].map(|at| (rgba[at] * 255.0).round() as u8)
    };
    let near =
        |[pr, pg, pb]: [u8; 3]| pr.abs_diff(r) < 24 && pg.abs_diff(g) < 24 && pb.abs_diff(b) < 24;
    let coloured = (0..470).any(|y| (0..width).any(|x| near(at(x, y))));
    assert!(coloured, "the code's keyword is coloured");
}

#[test]
fn a_chat_at_work_says_so_above_its_box_however_long_the_turn() {
    // A long turn still going, between tools: its top, where the line its
    // work folds under is, is far above the view.
    let mut node = crate::chat::fixtures::long_chat(2);
    let last = node.turns.len() - 1;
    node.turns[last].items.pop();
    let more = node.turns[last].items.clone();
    node.turns[last].items.extend(more);
    node.turns[last].sent = 2;
    let Some(pixels) = render_chat("chat-working", node, "", 1.0) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    // The dots, in the accent colour, just above the box, at the column's
    // left, where "Working · 12s" goes after them.
    let width = 720;
    let gold = (480..540).any(|y| {
        (0..120).any(|x| {
            let at = (y * width + x) * 4;
            let [r, g, b] = [pixels[at], pixels[at + 1], pixels[at + 2]];
            r > 0xb0 && g > 0x90 && b < 0x80
        })
    });
    assert!(gold, "the dots are drawn");
}

/// Whether any pixel of `pixels`, an image 720 wide, in rows `rows` and
/// columns `columns`, is near `colour`.
fn drawn_in(
    pixels: &[u8],
    (rows, columns): (std::ops::Range<usize>, std::ops::Range<usize>),
    colour: u32,
) -> bool {
    drawn_in_wide(pixels, 720, (rows, columns), colour)
}

/// As [`drawn_in`], in an image `wide` pixels wide.
fn drawn_in_wide(
    pixels: &[u8],
    wide: usize,
    (rows, columns): (std::ops::Range<usize>, std::ops::Range<usize>),
    colour: u32,
) -> bool {
    let [_, r, g, b] = colour.to_be_bytes();
    let near = |a: u8, b: u8| a.abs_diff(b) < 24;
    rows.into_iter().any(|y| {
        columns.clone().any(|x| {
            let at = (y * wide + x) * 4;
            near(pixels[at], r) && near(pixels[at + 1], g) && near(pixels[at + 2], b)
        })
    })
}

#[test]
fn the_context_meter_is_quiet_with_room_amber_as_it_fills_and_red_when_all_but_full() {
    // The circular context gauge immediately left of Send.
    let meter = (580..640, 590..638);
    let amber = 0xe8_9a_3c;
    let red = 0xe0_6c_75;
    let with = |used, five_hour: u8| {
        let mut node = chat_node(false);
        let usage = node.usage.as_mut().expect("usage");
        usage.used = used;
        usage.five_hour = Some(clackatoo_protocol::Limit {
            used: five_hour,
            resets: None,
        });
        node
    };
    let Some(quiet) = render_chat("chat-meter", with(38_400, 37), "", 1.0) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    assert!(!drawn_in(&quiet, meter.clone(), amber));
    assert!(!drawn_in(&quiet, meter.clone(), red));
    let filling = render_chat("chat-meter-amber", with(170_000, 37), "", 1.0).unwrap();
    assert!(drawn_in(&filling, meter.clone(), amber));
    assert!(!drawn_in(&filling, meter.clone(), red));
    let full = render_chat("chat-meter-red", with(194_000, 37), "", 1.0).unwrap();
    assert!(drawn_in(&full, meter.clone(), red));
    // A limit all but reached says so beside a context with room.
    let limit = render_chat("chat-meter-limit", with(38_400, 99), "", 1.0).unwrap();
    assert!(drawn_in(&limit, meter, red));
}

/// The daemon's own settings every chat has beside its agent's, as it
/// makes them: what the agent may do with clackatoo's tools, and whether
/// it drives the browser.
fn daemons_own_settings() -> Vec<clackatoo_protocol::Setting> {
    use clackatoo_protocol::{Setting, SettingChoice, SettingKind};
    let choices = |values: &[(&str, &str)]| {
        (values.iter())
            .map(|&(value, name)| SettingChoice {
                value: value.into(),
                name: name.into(),
                description: None,
            })
            .collect()
    };
    vec![
        Setting {
            id: "clackatoo.tools".into(),
            name: "Tools".into(),
            kind: SettingKind::Tools,
            value: "ask".into(),
            choices: choices(&[("ask", "Ask"), ("allow", "Allow"), ("off", "Off")]),
        },
        Setting {
            id: "clackatoo.browser".into(),
            name: "Browser".into(),
            kind: SettingKind::Browser,
            value: "off".into(),
            choices: choices(&[("off", "Off"), ("on", "On")]),
        },
    ]
}

#[test]
fn the_meter_fits_beside_the_daemons_own_chips_however_narrow_the_chat() {
    let amber = 0xe8_9a_3c;
    let mut node = chat_node(false);
    node.settings.extend(daemons_own_settings());
    let usage = node.usage.as_mut().expect("usage");
    usage.five_hour = Some(clackatoo_protocol::Limit {
        used: 91,
        resets: None,
    });
    for width in [720_usize, 560, 420] {
        #[allow(clippy::cast_precision_loss)]
        let wide = (1.0, None, width as f64);
        let name = format!("chat-meter-chips-{width}");
        let Some(pixels) = render_chat_wide(&name, node.clone(), "", wide, |_, _| {}) else {
            eprintln!("no GPU available; skipping");
            return;
        };
        // The limit, or the context or the bar carrying its colour, is
        // drawn in the row of controls, right of the middle.
        let row = (560..640, width / 2..width - 40);
        assert!(drawn_in_wide(&pixels, width, row, amber), "{width}");
    }
}

#[test]
fn the_meters_detail_shows_above_the_box_when_asked() {
    let Some(shut) = render_chat("chat-meter-shut", chat_node(false), "", 1.0) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let open = render_chat_with("chat-meter-detail", chat_node(false), "", 1.0, |chat| {
        assert!(chat.toggle_detail());
    })
    .unwrap();
    // Above the box, where the conversation was: the detail's panel.
    let above = 380 * 720 * 4..560 * 720 * 4;
    assert_ne!(shut[above.clone()], open[above]);
}

/// The tabs of a crowded window: fifteen, some with long names.
const CROWDED_TABS: [&str; 15] = [
    "zsh",
    "nvim crates/gui/src/chrome.rs",
    "cargo test --workspace",
    "Claude Code ⋯",
    "htop",
    "jj log -r @",
    "ssh box",
    "nvim README.md",
    "Codex ?",
    "make release",
    "python3 manage.py runserver",
    "tail -f daemon.log",
    "zsh",
    "lazygit",
    "man tmux",
];

/// The workspaces of a crowded window.
const CROWDED_WORKSPACES: [&str; 12] = [
    "clackatoo",
    "peios",
    "homelab",
    "dotfiles",
    "learn",
    "acta",
    "website",
    "notes",
    "scratch",
    "gcsl",
    "phone",
    "infra",
];

/// A window `width` wide at scale 1, its bars full: fifteen tabs with long
/// names, the eighth shown, a dozen workspaces, the second shown, and
/// `note`, of `kind`, with the mouse over what `hover` says. Returns its
/// size and frame, and what's drawn.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn crowded_bars(
    width: u32,
    note: (&str, NoteKind),
    hover: impl Fn(&Chrome) -> Hit,
) -> Option<([u32; 2], Chrome, Vec<u8>)> {
    crowded_bars_at(width, 1, note, hover)
}

/// As [`crowded_bars`] does, at `scale` times the size: `width` is in
/// the screen's pixels.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn crowded_bars_at(
    width: u32,
    scale: u32,
    (note, kind): (&str, NoteKind),
    hover: impl Fn(&Chrome) -> Hit,
) -> Option<([u32; 2], Chrome, Vec<u8>)> {
    let gpu = gpu()?;
    let mut fonts = fonts(f64::from(scale));
    let metrics = Metrics {
        cell: [fonts.cell.width as f32, fonts.cell.height as f32],
        ui_line: fonts.ui.height,
        padding: 4.0 * scale as f32,
    };
    let size = [width, 240 * scale];
    let geometry = Geometry::new(size, metrics);
    let buttons = ButtonLayout::default();
    let tabs = (1_u32..)
        .zip(CROWDED_TABS)
        .map(|(id, name)| (TabId(id), format!("{id} {name}"), id == 8))
        .collect();
    let workspaces = (1_u32..)
        .zip(CROWDED_WORKSPACES)
        .map(|(id, name)| WorkspaceName::plain(id, name, id as usize % 4 + 1, id == 2))
        .collect();
    let chrome = Chrome::new(
        geometry,
        Contents {
            workspaces,
            tabs,
            status: Status::Tabs {
                mode: Some(ModeName::Normal),
                note: Some(note.into()),
                kind,
                panes: 2,
            },
            buttons: &buttons,
            bottom_shown: true,
            menu: None,
            alert: None,
            rounded: false,
            finder: None,
            scrolls: Scrolls::default(),
            messages: None,
            waiting: None,
            which_keys: None,
            background: None,
        },
        &mut |text, bold| fonts.ui_width(text, bold),
    );
    let layout = Layout::Pane(Pane {
        id: PaneId(1),
        content: Content::Terminal(TerminalId(1)),
        home: None,
        key: None,
    });
    let placement = Placement::new(&layout, &geometry.area);
    let screen = screen(
        200,
        8,
        "$ cargo build\r\n   Compiling clackatoo v2026.10.35\r\n$ ",
    );
    let rows: Vec<_> = screen.rows.iter().collect();
    let scenes = [Scene {
        scroll: None,
        origin: placement.panes[0].1.origin,
        size: placement.panes[0].1.cells_size(geometry.area.cell),
        rows: &rows,
        cursor: None,
        normal_mode: true,
        highlights: &[],
    }];
    let painted = chrome.paint(hover(&chrome));
    let mut fills = vec![chrome::window_fill(&geometry, false)];
    fills.extend(painted.fills);
    fills.extend(chrome::paint_panes(&geometry, &placement, None, None));
    let mut texts = painted.texts;
    texts.extend(painted.over_texts);
    let picture = Picture {
        scenes: &scenes,
        fills: &fills,
        over: &painted.over,
        texts: &texts,
        glyphs: &[],
        over_glyphs: &[],
        images: &[],
        transparent: false,
    };
    let pixels = draw(&gpu, size, &picture, &mut fonts);
    Some((size, chrome, pixels))
}

/// A message too long for the bar of a narrow window.
const LONG_MESSAGE: &str = "this window can't read what the daemon says of chats: it's from a \
                            newer clackatoo, and :update updates this window";

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn crowded_bars_scroll_their_names_and_keep_messages_off_the_tabs() {
    let message = (LONG_MESSAGE, NoteKind::Message);
    let Some((size, chrome, pixels)) = crowded_bars(900, message, |_| Hit::Nothing) else {
        eprintln!("no GPU available; skipping");
        return;
    };
    let pixel = |pixels: &[u8], [x, y]: [f32; 2]| {
        let at = (y as usize * size[0] as usize + x as usize) * 4;
        pixels[at..at + 3].to_vec()
    };
    drop((chrome, pixels));
    for width in [900, 1400, 2000] {
        let (size, chrome, pixels) = crowded_bars(width, message, |_| Hit::Nothing).unwrap();
        save(&format!("bars-{width}"), size, &pixels);
        // The tab shown is in view, and those hidden are counted.
        assert!(
            chrome.tabs.iter().any(|tab| tab.active),
            "the tab shown shows at {width}"
        );
        assert!(chrome.hidden.iter().any(|count| count.bar == Bar::Bottom));
        // So is the workspace shown, the second.
        assert!(chrome.workspaces.iter().any(|item| item.active));
        // The message starts after the tabs and their `+`, cut short.
        let plus = chrome.new_tab.expect("a +");
        let note = chrome.notes.first().expect("the message");
        assert!(note.x >= plus.x + plus.width, "at {width}");
        if width < 2000 {
            assert!(
                chrome.cut.is_some() && note.text.ends_with('…'),
                "at {width}"
            );
        }
        let (size, _, pixels) =
            crowded_bars(width, ("↑3", NoteKind::Label), |_| Hit::Nothing).unwrap();
        save(&format!("bars-short-{width}"), size, &pixels);
    }
    // With the mouse over it, the message shows whole above the bar.
    let (_, chrome, hovered) = crowded_bars(900, message, |_| Hit::Note).unwrap();
    save("bars-hover-900", size, &hovered);
    let cut = chrome.cut.clone().unwrap();
    let raised = cut.raised(&chrome.geometry);
    assert!(raised.rect.y + raised.rect.height < chrome.geometry.bottom.y);
    assert_eq!(
        raised.lines.concat().replace(' ', ""),
        LONG_MESSAGE.replace(' ', "")
    );
    let (_, _, plain) = crowded_bars(900, message, |_| Hit::Nothing).unwrap();
    let inside = [raised.rect.x + 2.0, raised.rect.y + 2.0];
    assert_ne!(
        pixel(&hovered, inside),
        pixel(&plain, inside),
        "a box is drawn"
    );
    // A warning cut short shows whole for a moment without the mouse, in
    // the warning colour.
    let warning = (LONG_MESSAGE, NoteKind::Warning { notice: true });
    let (_, chrome, pixels) = crowded_bars(900, warning, |_| Hit::Nothing).unwrap();
    save("bars-warning-900", size, &pixels);
    let cut = chrome.cut.clone().unwrap();
    assert!(cut.raised(&chrome.geometry).warning);
    assert_ne!(
        pixel(&pixels, inside),
        pixel(&plain, inside),
        "a box is drawn"
    );
    // The mouse over a count of tabs hidden lights it up.
    let (size, _, pixels) = crowded_bars(1400, ("↑3", NoteKind::Label), |chrome| {
        let count = chrome.hidden.iter().find(|count| count.bar == Bar::Bottom);
        count.map_or(Hit::Nothing, |count| Hit::Hidden(count.bar, count.end))
    })
    .unwrap();
    save("bars-count-hover-1400", size, &pixels);
}

#[test]
fn crowded_bars_keep_the_tab_shown_however_narrow_and_at_twice_the_size() {
    let long = (LONG_MESSAGE, NoteKind::Message);
    let short = ("↑3", NoteKind::Label);
    if gpu().is_none() {
        eprintln!("no GPU available; skipping");
        return;
    }
    // Narrow windows at the usual size, and windows twice the size, as on
    // a screen scaled to 2×.
    for (width, scale) in [(360, 1), (430, 1), (720, 2), (860, 2), (1800, 2)] {
        for (name, note) in [("", long), ("-short", short)] {
            let (size, chrome, pixels) =
                crowded_bars_at(width, scale, note, |_| Hit::Nothing).unwrap();
            save(&format!("bars{name}-{width}-at-{scale}x"), size, &pixels);
            let at = format!("{width} px at {scale}x, {note:?}");
            let shown = chrome.tabs.iter().find(|tab| tab.active);
            let shown = shown.unwrap_or_else(|| panic!("{at}: the tab shown"));
            assert!(shown.rect.width > 0.0, "{at}");
            assert!(chrome.workspaces.iter().any(|item| item.active), "{at}");
            let plus = chrome.new_tab.expect("a +");
            for note in &chrome.notes {
                assert!(note.x >= plus.x + plus.width, "{at}");
            }
        }
    }
}

#[test]
fn renders_antigravity_allowances_in_wide_and_narrow_panes() {
    use clackatoo_protocol::{ProviderUsage, UsageLimit};
    let mut antigravity = chat_node(false);
    antigravity.provider_usage = vec![ProviderUsage {
        agent: "antigravity".into(),
        title: "Antigravity".into(),
        checked: 12_000,
        limits: [
            ("Gemini · Weekly", 1, Some(220_000_000)),
            ("Gemini · 5-hour", 4, Some(1_000_000)),
            ("Claude & GPT (shared) · Weekly", 0, None),
            ("Claude & GPT (shared) · 5-hour", 0, None),
            ("Gemini 4 Argon · Weekly", 15, Some(220_000_000)),
        ]
        .into_iter()
        .enumerate()
        .map(|(id, (label, used, resets))| UsageLimit {
            id: id.to_string(),
            label: label.into(),
            used,
            resets,
        })
        .collect(),
        ..Default::default()
    }];
    render_chat_sized(
        "usage-modal-antigravity",
        antigravity.clone(),
        "",
        (1.0, None, [1480.0, 1040.0]),
        |chat, _| {
            chat.usage_supported = true;
            chat.toggle_detail();
        },
    );
    render_chat_wide(
        "usage-modal-antigravity-narrow",
        antigravity,
        "",
        (1.0, None, 420.0),
        |chat, fonts| {
            chat.usage_supported = true;
            chat.toggle_detail();
            chat.paint(fonts, [0.0; 2], [420.0, 640.0], true);
            chat.scroll_by(10_000.0);
        },
    );
}

#[test]
fn renders_usage_modal_with_provider_windows_and_unavailable_states() {
    use clackatoo_protocol::{ProviderUsage, UsageLimit};
    let mut node = chat_node(false);
    node.provider_usage = vec![
        ProviderUsage {
            agent: "codex".into(),
            title: "Codex".into(),
            checked: 12_000,
            limits: vec![UsageLimit {
                id: "week".into(),
                label: "7-day".into(),
                used: 16,
                resets: Some(220_000_000),
            }],
            ..Default::default()
        },
        ProviderUsage {
            agent: "claude".into(),
            title: "Claude Code".into(),
            checked: 12_000,
            limits: vec![
                UsageLimit {
                    id: "five".into(),
                    label: "5-hour".into(),
                    used: 13,
                    resets: Some(1_000_000),
                },
                UsageLimit {
                    id: "week".into(),
                    label: "Weekly".into(),
                    used: 94,
                    resets: Some(220_000_000),
                },
                UsageLimit {
                    id: "model".into(),
                    label: "Weekly · Fable".into(),
                    used: 0,
                    resets: Some(220_000_000),
                },
            ],
            ..Default::default()
        },
        ProviderUsage {
            agent: "gemini".into(),
            title: "Gemini CLI".into(),
            notice: Some("This adapter does not expose subscription limits.".into()),
            ..Default::default()
        },
    ];
    render_chat_sized(
        "usage-modal-wide",
        node.clone(),
        "Keep this draft",
        (1.0, None, [1480.0, 1040.0]),
        |chat, _| {
            chat.usage_supported = true;
            chat.toggle_detail();
        },
    );
    let mut unavailable = node.clone();
    unavailable.provider_usage[0].limits[0].used = 100;
    unavailable.provider_usage[0].limits[0].resets = None;
    unavailable.provider_usage[1].limits[0].resets = Some(1);
    render_chat_wide(
        "usage-modal-expired",
        unavailable,
        "",
        (1.0, None, 720.0),
        |chat, _| {
            chat.usage_supported = true;
            chat.toggle_detail();
        },
    );
    for width in [720_u16, 420] {
        let Some(pixels) = render_chat_wide(
            &format!("usage-modal-{width}"),
            node.clone(),
            "Keep this draft",
            (1.0, None, f64::from(width)),
            |chat, _| {
                chat.usage_supported = true;
                assert!(chat.toggle_detail());
            },
        ) else {
            return;
        };
        assert!(pixels.chunks_exact(4).any(|p| p != &pixels[..4]));
        render_chat_wide(
            &format!("usage-modal-bottom-{width}"),
            node.clone(),
            "Keep this draft",
            (1.0, None, f64::from(width)),
            |chat, fonts| {
                chat.usage_supported = true;
                chat.toggle_detail();
                chat.paint(fonts, [0.0; 2], [f32::from(width), 640.0], true);
                chat.scroll_by(10_000.0);
            },
        )
        .unwrap();
    }
}
