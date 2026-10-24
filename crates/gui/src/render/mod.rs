//! Drawing the window with the GPU: its panes' screens, and the window's
//! own rectangles and text, such as its bars and the dividers between
//! panes.
//!
//! Each frame turns everything into two lists of instanced quads, solid
//! rectangles and glyphs, and draws them in four passes over one render
//! pass: backgrounds and the cursor, then glyphs, then underlines,
//! strikethroughs and cursors, then the window's own text on top. The
//! renderer draws into any texture, so the same code serves the window and
//! offscreen tests.

pub(crate) mod icons;
use icons::Icon;

use bytemuck::{Pod, Zeroable};
use clackatoo_protocol::{Attributes, Underline};
use clackatoo_term::Cell;

use crate::boxdraw;
use crate::colors::{Rgba, Theme};
use crate::fonts::{CellMetrics, FaceId, Fonts, Variant, is_regional_indicator, is_skin_tone};
use crate::glyphs::{ATLAS_SIZE, AtlasGlyph, GlyphCache};
use swash::GlyphId;

/// Everything one frame shows.
#[derive(Debug)]
pub(crate) struct Picture<'a> {
    /// The panes.
    pub(crate) scenes: &'a [Scene<'a>],
    /// Rectangles of the window's own, such as the tiles and the names in
    /// the bars, under the panes.
    pub(crate) fills: &'a [Fill],
    /// Rectangles of the window's own over the panes, such as the palette.
    pub(crate) over: &'a [Fill],
    /// Whether the window can be see-through, so that its background, a
    /// fill of its own, can round its corners. Otherwise it's cleared to
    /// the background colour.
    pub(crate) transparent: bool,
    /// Text of the window's own, such as the names in the bars, over
    /// everything.
    pub(crate) texts: &'a [Text<'a>],
    /// Glyphs laid out already, as a chat pane's text is, drawn over the
    /// fills and under the overlays.
    pub(crate) glyphs: &'a [PlacedGlyph],
    /// The same, over the overlays, as a chat's menu's text is.
    pub(crate) over_glyphs: &'a [PlacedGlyph],
    /// Pictures, as a browser pane's page is, over the panes'
    /// backgrounds and under their text.
    pub(crate) images: &'a [PlacedImage<'a>],
}

/// The most a picture is drawn, in pixels each way: a page's picture is
/// no bigger than the screen it's drawn on, and a bigger one isn't drawn.
pub(crate) const MAX_IMAGE_SIDE: u32 = 4096;

/// A picture, decoded, as the GPU takes it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Image {
    /// Its number, never reused, so the renderer uploads each picture
    /// once however many frames draw it.
    pub(crate) id: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Its pixels, row by row from the top, four bytes each: red, green,
    /// blue and alpha.
    pub(crate) rgba: Vec<u8>,
}

/// A picture placed in the window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlacedImage<'a> {
    /// What it's kept on the GPU by from one frame to the next, as the
    /// pane showing it: one texture each.
    pub(crate) slot: u64,
    /// Where it goes, left, top, width and height, in pixels: it's
    /// stretched to fill it.
    pub(crate) rect: [f32; 4],
    pub(crate) image: &'a Image,
    /// Clip in window coordinates, for scrolling chat images.
    pub(crate) clip: Option<[f32; 4]>,
}

/// A chat image kept alive between layout and the GPU pass.
#[derive(Debug)]
pub(crate) struct PaintedImage {
    pub(crate) image: std::sync::Arc<Image>,
    pub(crate) rect: [f32; 4],
    pub(crate) clip: [f32; 4],
}

impl PaintedImage {
    pub(crate) fn placed(&self) -> PlacedImage<'_> {
        PlacedImage {
            slot: self.image.id,
            rect: self.rect,
            image: &self.image,
            clip: Some(self.clip),
        }
    }
}

/// A glyph laid out already: which, at what size, and where.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlacedGlyph {
    pub(crate) face: FaceId,
    pub(crate) glyph: GlyphId,
    /// The size, in pixels per em.
    pub(crate) ppem: f32,
    /// Its pen: the left of its advance, on the baseline, in pixels.
    pub(crate) pen: [f32; 2],
    pub(crate) ink: Ink,
    /// What it's cut off outside: left, top, width and height.
    pub(crate) clip: [f32; 4],
}

/// A rectangle of the window's own: perhaps rounded, perhaps bordered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fill {
    /// A vector icon in this rectangle, drawn from the glyph atlas.
    pub(crate) icon: Option<Icon>,
    /// Left, top, width and height, in pixels.
    pub(crate) rect: [f32; 4],
    pub(crate) paint: Paint,
    /// How round its corners are, in pixels.
    pub(crate) radius: f32,
    /// A border inside its edge: how thick, and its colour.
    pub(crate) border: Option<(f32, Paint)>,
    /// What it's cut off outside, if anything: left, top, width and
    /// height.
    pub(crate) clip: Option<[f32; 4]>,
}

impl Fill {
    /// A square-cornered, borderless rectangle.
    pub(crate) fn solid(rect: [f32; 4], paint: Paint) -> Self {
        Self {
            rect,
            paint,
            icon: None,
            radius: 0.0,
            border: None,
            clip: None,
        }
    }

    /// A rectangle with corners `radius` round.
    pub(crate) fn rounded(rect: [f32; 4], paint: Paint, radius: f32) -> Self {
        Self {
            rect,
            paint,
            icon: None,
            radius,
            border: None,
            clip: None,
        }
    }

    pub(crate) fn icon(rect: [f32; 4], icon: Icon, paint: Paint) -> Self {
        Self {
            icon: Some(icon),
            ..Self::solid(rect, paint)
        }
    }

    /// With a border `width` thick, in `paint`.
    pub(crate) fn bordered(self, width: f32, paint: Paint) -> Self {
        Self {
            border: Some((width, paint)),
            ..self
        }
    }

    /// Cut off outside `clip`: left, top, width and height.
    pub(crate) fn clipped(self, clip: [f32; 4]) -> Self {
        Self {
            clip: Some(clip),
            ..self
        }
    }
}

/// The colours the window's own things are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    /// Nothing: for a border alone.
    Clear,
    /// The bars, and the window behind everything.
    Bar,
    /// The faint line round the window.
    WindowEdge,
    /// A soft shadow, under something raised.
    Shadow,
    /// Behind a pane, the colour its terminal's background is.
    Tile,
    /// The lines between panes, and between them and the bars.
    Divider,
    /// The focused pane's edge, when there are others.
    FocusEdge,
    /// Behind what's picked, such as the workspace shown...
    Picked,
    /// ...and its edge, in the top bar.
    PickedEdge,
    /// Behind the row picked in a list raised over the panes: the
    /// finder's, and the line its preview is of, a menu's, a chat's
    /// settings', the command line's or a completion menu's.
    Chosen,
    /// A quieter selection within the chat composer.
    ChatChosen,
    Claude,
    Gemini,
    /// The dot by a workspace's name, by its place in the list.
    Workspace(u8),
    /// clackatoo's own colour, for what it shows over the panes.
    Accent,
    /// Behind the mode's name.
    Mode(crate::chrome::ModeName),
    /// Under the mouse.
    Hover,
    /// Drawn icons.
    Icon,
    /// Drawn icons under the mouse, in the text colour.
    Ink,
    /// The command palette...
    Palette,
    /// ...and its edge.
    PaletteEdge,
    /// Behind what's gone wrong, faintly in the warning colour...
    Warning,
    /// ...and its edge.
    WarningEdge,
    /// Behind what's raised off a pane, such as a message sent in a chat,
    /// or the box a message is typed in...
    Raised,
    /// ...and its edge.
    RaisedEdge,
    /// Behind code, in a chat.
    Code,
    /// Behind a line added to a file, faintly green...
    Added,
    /// ...and one removed, faintly red.
    Removed,
    /// Behind text selected, in a chat, as a terminal's selection is.
    Selected,
    /// A gauge's filling while there's room to spare, quietly...
    Gauge,
    /// ...in amber once it's getting full...
    Caution,
    /// ...and in the warning colour once it's all but full.
    Full,
}

/// A line of text of the window's own.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Text<'a> {
    /// The top left corner of its line, in pixels.
    pub(crate) origin: [f32; 2],
    pub(crate) content: &'a str,
    pub(crate) ink: Ink,
    pub(crate) font: TextFont,
}

/// How text of the window's own is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ink {
    /// In the text colour.
    Plain,
    /// In a quieter colour, for what matters less.
    Muted,
    /// Bold, in the bars' colour, on something coloured.
    OnColor,
    /// In the warning colour, for what's gone wrong.
    Warning,
    /// In amber, for what's getting close to going wrong.
    Caution,
    /// In clackatoo's own colour, for focus and what to act on, such as a
    /// chat waiting for you.
    Accent,
    /// In the links' colour, the accent blue.
    Link,
    /// In the colour of letters what's typed matched, as in the finder.
    Matched,
    /// Bold, in clackatoo's own colour, as a key to press is.
    Key,
    /// Green, for what's added to a file...
    Added,
    /// ...and red, for what's removed.
    Removed,
    /// Bold, in the text colour, as a heading is.
    Bold,
    /// In a terminal's colour, as the theme draws it, as an editor's text
    /// is drawn.
    Color(clackatoo_protocol::Color),
}

/// Which font text of the window's own is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextFont {
    /// The terminal's, a cell per character, or two for a wide one, as
    /// for commands being typed.
    Mono,
    /// The interface font, as for names in the bars.
    Ui,
}

/// How many cells `ch` takes in text drawn in [`TextFont::Mono`].
pub(crate) fn mono_cells(ch: char) -> usize {
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1)
}

/// How many cells `text` takes in [`TextFont::Mono`].
pub(crate) fn mono_width(text: &str) -> usize {
    text.chars().map(mono_cells).sum()
}

/// What one pane shows, and where.
#[derive(Debug)]
pub(crate) struct Scene<'a> {
    /// Where the pane's top left corner is, in pixels.
    pub(crate) origin: [f32; 2],
    /// How big the pane is, in pixels. Rows and cells past it aren't drawn.
    pub(crate) size: [f32; 2],
    /// The rows to show, top to bottom.
    pub(crate) rows: &'a [&'a clackatoo_term::Row],
    pub(crate) cursor: Option<SceneCursor>,
    /// Whether the pane is in normal mode, which draws its cursor in the
    /// accent colour.
    pub(crate) normal_mode: bool,
    /// Stretches of text to highlight.
    pub(crate) highlights: &'a [Highlight],
}

/// A stretch of one line to highlight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Highlight {
    /// The line in view, from the top.
    pub(crate) line: usize,
    pub(crate) cols: std::ops::Range<usize>,
    pub(crate) kind: HighlightKind,
}

/// How long text just copied stays highlighted, so you see what went, in
/// terminals and editors alike: as long as Neovim's highlight on yank.
pub(crate) const FLASH: std::time::Duration = std::time::Duration::from_millis(150);

/// Why text is highlighted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HighlightKind {
    /// It's selected.
    Selection,
    /// A search found it.
    Match,
    /// It was just copied, for [`FLASH`].
    Copied,
    /// It's the cursor's line (`cursorline`).
    CursorLine,
    /// It's a column marked down an editor (`colorcolumn`).
    ColorColumn,
    /// It's the link under the mouse, which a click opens: underlined.
    Link,
    /// It's the step of a lesson being done.
    LessonStep,
    /// It's a step of a lesson that's done.
    LessonDone,
    /// It's code in a lesson's prose: on the faint background the chat
    /// draws code on.
    Code,
    /// It's a line added, as a review shows a change: on the faint green
    /// the chat's cards show one on.
    Added,
    /// It's a line removed: on faint red.
    Removed,
    /// It's a use of the name the cursor rests on, where it's read, or
    /// the server doesn't say (`highlightuses`).
    Used,
    /// It's a use of the name the cursor rests on, where it's written: a
    /// little brighter.
    Written,
}

impl HighlightKind {
    /// Whether it's drawn behind the text's own backgrounds and the
    /// cursor, as a mark of where things are rather than of the text.
    fn behind(self) -> bool {
        matches!(
            self,
            Self::CursorLine
                | Self::ColorColumn
                | Self::LessonStep
                | Self::LessonDone
                | Self::Code
                | Self::Added
                | Self::Removed
                | Self::Used
                | Self::Written
        )
    }
}

/// Where the cursor is drawn, and how.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SceneCursor {
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) style: CursorStyle,
}

/// How the cursor is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CursorStyle {
    /// Covering the cell, with its character drawn in the background
    /// colour.
    Block,
    /// A line under the cell.
    Underline,
    /// A bar at the cell's left edge.
    Bar,
    /// An outline of the cell, while the window isn't focused, or one of
    /// its own prompts has the keys.
    Hollow,
}

/// Space between the window's edge and the grid, in physical pixels per
/// logical pixel.
pub(crate) const PADDING: f64 = 4.0;

/// A solid rectangle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Rect {
    origin: [f32; 2],
    size: [f32; 2],
    color: Rgba,
    /// How round its corners are, in pixels.
    radius: f32,
    /// How thick its border is, in pixels, inside its edge.
    border: f32,
    border_color: Rgba,
    /// Left, top, right and bottom: it's cut off outside.
    clip: [f32; 4],
}

/// A clip box that cuts nothing off.
const NO_CLIP: [f32; 4] = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];

/// Draws an underline of `shape` under `width` pixels from `left`, its top
/// at `top`, with lines `stroke` thick, by giving `push` each rectangle's
/// origin and size. Dots, dashes and waves are placed by where they are in
/// the window, not in the cell, so they run on unbroken from cell to cell.
fn underline(
    shape: Underline,
    [left, top]: [f32; 2],
    width: f32,
    stroke: f32,
    mut push: impl FnMut([f32; 2], [f32; 2]),
) {
    let right = left + width;
    // Repeats `size` wide every `every` pixels, from the first whole one.
    let mut repeat = |size: f32, every: f32| {
        let mut x = (left / every).ceil() * every;
        while x < right {
            push([x, top], [size.min(right - x), stroke]);
            x += every;
        }
    };
    match shape {
        Underline::None => {}
        Underline::Single => push([left, top], [width, stroke]),
        Underline::Double => {
            push([left, top], [width, stroke]);
            push([left, top + 2.0 * stroke], [width, stroke]);
        }
        Underline::Dotted => repeat(stroke, 2.0 * stroke),
        Underline::Dashed => repeat(3.0 * stroke, 5.0 * stroke),
        Underline::Curly => {
            // A sine wave, a pixel column at a time, each column reaching
            // from where the wave is to where it goes next, so steep parts
            // have no gaps.
            let height = stroke.max(1.0);
            let period = 6.0 * height;
            let wave = |x: f32| top + height * (1.0 - (std::f32::consts::TAU * x / period).sin());
            let mut x = left.floor();
            while x < right {
                let (here, next) = (wave(x), wave(x + 1.0));
                push([x, here.min(next)], [1.0, (here - next).abs() + stroke]);
                x += 1.0;
            }
        }
    }
}

/// A clip box from a left, top, width and height.
fn clip_box([left, top, width, height]: [f32; 4]) -> [f32; 4] {
    [left, top, left + width, top + height]
}

impl Rect {
    /// A square-cornered rectangle of one colour.
    fn solid(origin: [f32; 2], size: [f32; 2], color: Rgba) -> Self {
        Self {
            origin,
            size,
            color,
            radius: 0.0,
            border: 0.0,
            border_color: [0.0; 4],
            clip: NO_CLIP,
        }
    }
}

/// A glyph, drawn from the atlas.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Glyph {
    origin: [f32; 2],
    size: [f32; 2],
    uv_origin: [f32; 2],
    uv_size: [f32; 2],
    color: Rgba,
    /// 1 if the glyph has its own colours.
    colored: u32,
    /// Left, top, right and bottom: it's cut off outside.
    clip: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport: [f32; 2],
    atlas_size: [f32; 2],
}

/// A picture's quad: where it goes, in pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ImageQuad {
    origin: [f32; 2],
    size: [f32; 2],
    uv_origin: [f32; 2],
    uv_size: [f32; 2],
}

/// A picture on the GPU, in a slot.
#[derive(Debug)]
struct GpuImage {
    /// The picture's number.
    id: u64,
    size: [u32; 2],
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// The GPU objects that draw a terminal.
#[derive(Debug)]
pub(crate) struct Renderer {
    rect_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// The pictures drawn last frame, by slot.
    images: std::collections::HashMap<u64, GpuImage>,
    /// The most a picture is drawn, each way: [`MAX_IMAGE_SIDE`], or less
    /// if the GPU takes no bigger texture.
    image_side: u32,
    image_buffer: InstanceBuffer,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    atlas: wgpu::Texture,
    rect_buffer: InstanceBuffer,
    glyph_buffer: InstanceBuffer,
    glyphs: GlyphCache,
    theme: Theme,
    /// Instances built each frame, kept to reuse their memory.
    rects: Vec<Rect>,
    overlays: Vec<Rect>,
    glyph_instances: Vec<Glyph>,
    /// Glyphs drawn over the overlays, such as a label's text.
    text_glyphs: Vec<Glyph>,
    /// Each character's glyph in each variant, found once: finding it
    /// means asking the fonts, then the atlas.
    char_glyphs: CharGlyphs,
    /// The glyphs each cell text of several codepoints is shaped into, by
    /// variant, with where they go from the pen: `None` if it can't be.
    shaped: [std::collections::HashMap<Box<str>, Option<Shaped>>; 4],
    /// Holds a cell's text when it has more than one codepoint.
    cell_text: String,
}

/// A cell text's shaped glyphs, with where each goes from the pen.
type Shaped = Box<[(AtlasGlyph, [f32; 2])]>;

/// Glyphs by character and variant, with ASCII in a table of its own, as
/// it's nearly everything a terminal shows.
#[derive(Debug)]
struct CharGlyphs {
    ascii: Box<[[Cached; 4]; 128]>,
    other: std::collections::HashMap<(char, Variant), Option<AtlasGlyph>>,
}

/// A table entry: looked up or not, and if so, the glyph, if any.
#[derive(Clone, Copy, Debug)]
enum Cached {
    Unknown,
    Known(Option<AtlasGlyph>),
}

impl CharGlyphs {
    fn new() -> Self {
        Self {
            ascii: Box::new([[Cached::Unknown; 4]; 128]),
            other: std::collections::HashMap::new(),
        }
    }
}

impl Renderer {
    /// A renderer drawing into textures of `format`.
    #[allow(clippy::too_many_lines, reason = "one GPU object after another")]
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let layout = bind_group_layout(device);
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminal"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terminal"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = |label, entry_points, attributes: &[wgpu::VertexAttribute], stride| {
            create_pipeline(
                device,
                &pipeline_layout,
                &shader,
                format,
                label,
                entry_points,
                (attributes, stride),
            )
        };
        let rect_pipeline = pipeline(
            "rects",
            ("rect_vs", "rect_fs"),
            &wgpu::vertex_attr_array![
                0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32,
                4 => Float32, 5 => Float32x4, 6 => Float32x4,
            ],
            std::mem::size_of::<Rect>() as u64,
        );
        let glyph_pipeline = pipeline(
            "glyphs",
            ("glyph_vs", "glyph_fs"),
            &wgpu::vertex_attr_array![
                0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2,
                4 => Float32x4, 5 => Uint32, 6 => Float32x4,
            ],
            std::mem::size_of::<Glyph>() as u64,
        );
        let image_pipeline = pipeline(
            "images",
            ("image_vs", "image_fs"),
            &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2],
            std::mem::size_of::<ImageQuad>() as u64,
        );
        Self {
            rect_pipeline,
            glyph_pipeline,
            image_pipeline,
            layout,
            sampler,
            images: std::collections::HashMap::new(),
            image_side: MAX_IMAGE_SIDE.min(device.limits().max_texture_dimension_2d),
            image_buffer: InstanceBuffer::new("images"),
            bind_group,
            uniforms,
            atlas,
            rect_buffer: InstanceBuffer::new("rects"),
            glyph_buffer: InstanceBuffer::new("glyphs"),
            glyphs: GlyphCache::new(),
            theme: Theme::default(),
            rects: Vec::new(),
            overlays: Vec::new(),
            glyph_instances: Vec::new(),
            text_glyphs: Vec::new(),
            char_glyphs: CharGlyphs::new(),
            shaped: Default::default(),
            cell_text: String::new(),
        }
    }

    /// Throws away every rasterised glyph, after the text size changed.
    pub(crate) fn forget_glyphs(&mut self) {
        self.glyphs = GlyphCache::new();
        self.char_glyphs = CharGlyphs::new();
        self.shaped = Default::default();
    }

    /// The glyph for `ch` in `variant`, if any font has it.
    fn char_glyph(&mut self, fonts: &mut Fonts, ch: char, variant: Variant) -> Option<AtlasGlyph> {
        let glyphs = &mut self.glyphs;
        let mut find = || {
            let (face, glyph) = fonts.glyph(ch, variant)?;
            glyphs.get(fonts, face, glyph)
        };
        match usize::try_from(u32::from(ch)) {
            Ok(code) if code < 128 => {
                let slot = &mut self.char_glyphs.ascii[code][variant as usize];
                if let Cached::Known(glyph) = *slot {
                    return glyph;
                }
                let glyph = find();
                *slot = Cached::Known(glyph);
                glyph
            }
            _ => *self
                .char_glyphs
                .other
                .entry((ch, variant))
                .or_insert_with(find),
        }
    }

    /// Draws `picture` into `target`, which is `size` physical pixels.
    pub(crate) fn render(
        &mut self,
        gpu: (&wgpu::Device, &wgpu::Queue),
        target: &wgpu::TextureView,
        size: [u32; 2],
        picture: &Picture<'_>,
        fonts: &mut Fonts,
    ) {
        let (device, queue) = gpu;
        self.build(picture, fonts);
        for upload in self.glyphs.uploads.drain(..) {
            let [x, y, width, height] = upload.rect;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload.pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
        #[allow(clippy::cast_precision_loss)]
        let uniforms = Uniforms {
            viewport: [size[0] as f32, size[1] as f32],
            atlas_size: [ATLAS_SIZE as f32; 2],
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
        let images = self.upload_images(device, queue, picture.images);
        let background_rects = self.rects.len();
        self.rects.append(&mut self.overlays);
        let grid_glyphs = self.glyph_instances.len();
        self.glyph_instances.append(&mut self.text_glyphs);
        self.rect_buffer
            .write(device, queue, bytemuck::cast_slice(&self.rects));
        self.glyph_buffer
            .write(device, queue, bytemuck::cast_slice(&self.glyph_instances));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let clear = if picture.transparent {
                [0.0; 4]
            } else {
                self.theme.bar
            };
            let [r, g, b, a] = clear.map(f64::from);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_bind_group(0, &self.bind_group, &[]);
            let count = |n: usize| u32::try_from(n).expect("far fewer than 4 billion instances");
            if let Some(buffer) = &self.rect_buffer.buffer {
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..4, 0..count(background_rects));
            }
            self.draw_images(&mut pass, &images);
            if let Some(buffer) = &self.glyph_buffer.buffer {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..4, 0..count(grid_glyphs));
            }
            if let Some(buffer) = &self.rect_buffer.buffer
                && self.rects.len() > background_rects
            {
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..4, count(background_rects)..count(self.rects.len()));
            }
            if let Some(buffer) = &self.glyph_buffer.buffer
                && self.glyph_instances.len() > grid_glyphs
            {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..4, count(grid_glyphs)..count(self.glyph_instances.len()));
            }
        }
        queue.submit([encoder.finish()]);
    }

    /// Puts each of `placed` on the GPU, unless it's there already, and
    /// their quads in the image buffer, letting go of pictures no longer
    /// drawn. Returns the slots drawn, in the quads' order.
    fn upload_images(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        placed: &[PlacedImage<'_>],
    ) -> Vec<u64> {
        self.images
            .retain(|slot, _| placed.iter().any(|placed| placed.slot == *slot));
        let mut quads = Vec::with_capacity(placed.len());
        let mut slots = Vec::with_capacity(placed.len());
        for placed in placed {
            let image = placed.image;
            let expected = (image.width as usize) * (image.height as usize) * 4;
            let too_big = image.width > self.image_side || image.height > self.image_side;
            if image.width == 0 || image.height == 0 || too_big || image.rgba.len() != expected {
                continue;
            }
            let size = [image.width, image.height];
            let stale = (self.images.get(&placed.slot)).is_none_or(|kept| kept.size != size);
            if stale {
                let gpu = self.new_image(device, size);
                self.images.insert(placed.slot, gpu);
            }
            let Some(gpu) = self.images.get_mut(&placed.slot) else {
                continue;
            };
            if stale || gpu.id != image.id {
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &gpu.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &image.rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(image.width * 4),
                        rows_per_image: Some(image.height),
                    },
                    wgpu::Extent3d {
                        width: image.width,
                        height: image.height,
                        depth_or_array_layers: 1,
                    },
                );
                gpu.id = image.id;
            }
            let [left, top, width, height] = placed.rect;
            if width <= 0.0 || height <= 0.0 {
                continue;
            }
            let clipped = placed.clip.map_or(placed.rect, |clip| {
                crate::markdown::layout::intersection(placed.rect, clip)
            });
            let [x, y, w, h] = clipped;
            if w <= 0.0 || h <= 0.0 {
                continue;
            }
            quads.push(ImageQuad {
                origin: [x, y],
                size: [w, h],
                uv_origin: [(x - left) / width, (y - top) / height],
                uv_size: [w / width, h / height],
            });
            slots.push(placed.slot);
        }
        self.image_buffer
            .write(device, queue, bytemuck::cast_slice(&quads));
        slots
    }

    /// Draws the pictures in `slots`, in the image buffer's order, each
    /// with its own texture bound, then binds the atlas again.
    fn draw_images(&self, pass: &mut wgpu::RenderPass<'_>, slots: &[u64]) {
        let Some(buffer) = self
            .image_buffer
            .buffer
            .as_ref()
            .filter(|_| !slots.is_empty())
        else {
            return;
        };
        let count = |n: usize| u32::try_from(n).expect("far fewer than 4 billion pictures");
        pass.set_pipeline(&self.image_pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        for (index, slot) in slots.iter().enumerate() {
            if let Some(image) = self.images.get(slot) {
                pass.set_bind_group(0, &image.bind_group, &[]);
                pass.draw(0..4, count(index)..count(index + 1));
            }
        }
        pass.set_bind_group(0, &self.bind_group, &[]);
    }

    /// A texture `size` pixels, and what draws it.
    fn new_image(&self, device: &wgpu::Device, [width, height]: [u32; 2]) -> GpuImage {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        GpuImage {
            id: u64::MAX,
            size: [width, height],
            texture,
            bind_group,
        }
    }

    /// Turns the picture into rectangles and glyphs.
    pub(crate) fn build(&mut self, picture: &Picture<'_>, fonts: &mut Fonts) {
        self.rects.clear();
        self.overlays.clear();
        self.glyph_instances.clear();
        self.text_glyphs.clear();
        for scene in picture.scenes {
            self.push_scene(scene, fonts);
        }
        let rect = |fill: &Fill| {
            let [left, top, width, height] = fill.rect;
            let (border, border_color) = fill.border.map_or((0.0, [0.0; 4]), |(width, paint)| {
                (width, self.theme.paint(paint))
            });
            Rect {
                origin: [left, top],
                size: [width, height],
                color: self.theme.paint(fill.paint),
                radius: fill.radius,
                border,
                border_color,
                clip: fill.clip.map_or(NO_CLIP, clip_box),
            }
        };
        let (under, over): (Vec<Rect>, Vec<Rect>) = (
            picture
                .fills
                .iter()
                .filter(|fill| fill.icon.is_none())
                .map(rect)
                .collect(),
            picture
                .over
                .iter()
                .filter(|fill| fill.icon.is_none())
                .map(rect)
                .collect(),
        );
        // The tiles go under the panes' cells, and the palette over them.
        self.rects.splice(0..0, under);
        self.overlays.extend(over);
        for (fills, over) in [(picture.fills, false), (picture.over, true)] {
            for fill in fills {
                if let Some(icon) = fill.icon
                    && let Some(glyph) = self.icon(icon, fill)
                {
                    if over {
                        self.text_glyphs.push(glyph);
                    } else {
                        self.glyph_instances.push(glyph);
                    }
                }
            }
        }
        for text in picture.texts {
            self.push_text(text, fonts);
        }
        for placed in picture.glyphs {
            if let Some(glyph) = self.placed(placed, fonts) {
                self.glyph_instances.push(glyph);
            }
        }
        for placed in picture.over_glyphs {
            if let Some(glyph) = self.placed(placed, fonts) {
                self.text_glyphs.push(glyph);
            }
        }
    }

    fn icon(&mut self, icon: Icon, fill: &Fill) -> Option<Glyph> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let size = fill.rect[2].min(fill.rect[3]).round().clamp(1.0, 256.0) as u16;
        let atlas = self.glyphs.icon(icon, size)?;
        #[allow(clippy::cast_precision_loss)]
        let uv = [atlas.rect[0] as f32, atlas.rect[1] as f32];
        Some(Glyph {
            origin: [fill.rect[0].round(), fill.rect[1].round()],
            size: [f32::from(size); 2],
            uv_origin: uv,
            uv_size: [f32::from(size); 2],
            color: self.theme.paint(fill.paint),
            colored: 0,
            clip: fill.clip.map_or(NO_CLIP, clip_box),
        })
    }

    /// A glyph laid out already, ready to draw.
    fn placed(&mut self, placed: &PlacedGlyph, fonts: &mut Fonts) -> Option<Glyph> {
        let atlas = self
            .glyphs
            .get_sized(fonts, placed.face, placed.glyph, placed.ppem)?;
        let color = self.ink(placed.ink);
        // Whole pixels, so glyphs stay sharp. An emoji, which `place` fits
        // to a box, gets one an em tall, its baseline most of the way down.
        let pen = [placed.pen[0].round(), placed.pen[1].round()];
        let mut glyph = place(&atlas, pen, [placed.ppem; 2], color, placed.ppem * 0.8);
        glyph.clip = clip_box(placed.clip);
        Some(glyph)
    }

    /// The colour text in `ink` is drawn in.
    fn ink(&self, ink: Ink) -> Rgba {
        match ink {
            Ink::Plain | Ink::Bold => self.theme.foreground,
            Ink::OnColor => self.theme.bar,
            Ink::Muted => self.theme.muted,
            Ink::Warning => self.theme.warning,
            Ink::Caution => self.theme.caution,
            Ink::Accent | Ink::Key => self.theme.accent,
            Ink::Link => self.theme.link,
            Ink::Matched => self.theme.matched,
            Ink::Added => self.theme.added,
            Ink::Removed => self.theme.removed,
            Ink::Color(color) => self.theme.resolve(color).unwrap_or(self.theme.foreground),
        }
    }

    /// Adds one pane's cells, cursor, highlights and frame.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn push_scene(&mut self, scene: &Scene<'_>, fonts: &mut Fonts) {
        let cell = fonts.cell;
        let (cell_width, cell_height) = (cell.width as f32, cell.height as f32);
        let [origin_x, origin_y] = scene.origin;
        // Whole cells only: a terminal wider or taller than its pane, as
        // while it's being resized, is cut off.
        let cols = (scene.size[0] / cell_width).round() as usize;
        let lines = (scene.size[1] / cell_height).round() as usize;
        let cursor = scene.cursor;
        self.push_highlights(scene, true, [cols, lines], &cell);
        // Whether the row before wrapped halfway through a flag.
        let mut mid_flag = false;
        for (line, row) in scene.rows.iter().take(lines).enumerate() {
            let top = origin_y + line as f32 * cell_height;
            let baseline = top + cell.baseline;
            // Cells before `run_until` belong to a run that starts to their
            // left: drawn with it as one glyph if `run_drawn`, or else each
            // on its own.
            let (mut run_until, mut run_drawn) = (0, false);
            // The second half of a flag the wrap cut in two stands alone,
            // rather than pairing with the letter after it.
            if mid_flag && is_regional_indicator(row.cells()[0].ch()) {
                run_until = 1;
            }
            for (col, &cell_value) in row.cells().iter().enumerate().take(cols) {
                if cell_value.is_wide_spacer() {
                    continue;
                }
                let cells = if cell_value.is_wide() { 2.0 } else { 1.0 };
                let left = origin_x + col as f32 * cell_width;
                let style = row.style(cell_value);
                let mut colors = self.theme.cell(style);
                if let Some(cursor) =
                    cursor.filter(|cursor| cursor.line == line && cursor.col == col)
                {
                    let color = if scene.normal_mode {
                        self.theme.accent
                    } else {
                        self.theme.cursor
                    };
                    let bounds = ([left, top], [cells * cell_width, cell_height]);
                    if self.push_cursor(cursor.style, bounds, color, cell.stroke) {
                        colors.background = Some(color);
                        colors.foreground = self.theme.background;
                    }
                }
                if let Some(background) = colors.background {
                    self.rects.push(Rect::solid(
                        [left, top],
                        [cells * cell_width, cell_height],
                        background,
                    ));
                }
                let variant = Variant::new(
                    style.attributes.contains(Attributes::BOLD),
                    style.attributes.contains(Attributes::ITALIC),
                );
                let alone = if col < run_until {
                    !run_drawn
                } else if cell_value.ch() >= FIRST_JOINING
                    && let Some(end) = joined_run(row, col, cols)
                {
                    // A run the font can't join is still a run, so its
                    // cells don't pair with others: 🇽🇬🇧 is 🇽, 🇬, then 🇧.
                    run_until = end;
                    run_drawn = self.push_joined_text(
                        fonts,
                        row,
                        col..end,
                        variant,
                        [left, baseline],
                        colors.foreground,
                    );
                    !run_drawn
                } else {
                    true
                };
                if alone {
                    self.push_cell_text(
                        fonts,
                        row,
                        col,
                        cell_value,
                        variant,
                        [left, baseline],
                        [cells * cell_width, cell_height],
                        colors.foreground,
                    );
                }
                let width = cells * cell_width;
                let overlays = &mut self.overlays;
                underline(
                    style.underline,
                    [left, baseline + cell.underline_offset],
                    width,
                    cell.stroke,
                    |origin, size| overlays.push(Rect::solid(origin, size, colors.underline)),
                );
                if style.attributes.contains(Attributes::STRIKETHROUGH) {
                    self.overlays.push(Rect::solid(
                        [left, baseline - cell.strikeout_offset - cell.stroke / 2.0],
                        [width, cell.stroke],
                        colors.foreground,
                    ));
                }
            }
            mid_flag = ends_mid_flag(row, mid_flag);
        }
        self.push_highlights(scene, false, [cols, lines], &cell);
    }

    /// Adds a pane's highlights drawn `behind` its cells, or those over
    /// them, in a pane `cols` by `lines` of cells measuring `cell`.
    #[allow(clippy::cast_precision_loss)]
    fn push_highlights(
        &mut self,
        scene: &Scene<'_>,
        behind: bool,
        [cols, lines]: [usize; 2],
        cell: &CellMetrics,
    ) {
        let (cell_width, cell_height) = (cell.width as f32, cell.height as f32);
        let [origin_x, origin_y] = scene.origin;
        for highlight in scene.highlights {
            if highlight.line >= lines || highlight.kind.behind() != behind {
                continue;
            }
            let start = highlight.cols.start.min(cols);
            let end = highlight.cols.end.min(cols);
            let left = origin_x + start as f32 * cell_width;
            let top = origin_y + highlight.line as f32 * cell_height;
            let width = (end - start) as f32 * cell_width;
            let color = self.theme.highlight(highlight.kind);
            if highlight.kind == HighlightKind::Link {
                // A line under the text, as where text is underlined.
                let under = top + cell.baseline + cell.underline_offset;
                let stroke = cell.stroke.max(1.0);
                self.overlays
                    .push(Rect::solid([left, under], [width, stroke], color));
                continue;
            }
            self.rects
                .push(Rect::solid([left, top], [width, cell_height], color));
        }
    }

    /// Draws a cursor of `style` over the cell at `origin` of `size`.
    /// Returns whether it's a block, which the cell's own colours show by
    /// swapping instead.
    fn push_cursor(
        &mut self,
        style: CursorStyle,
        ([left, top], [width, height]): ([f32; 2], [f32; 2]),
        color: Rgba,
        stroke: f32,
    ) -> bool {
        // Bars and underlines are a little thicker than lines of text, so
        // they're easy to find.
        let thick = (width / 8.0).round().max(stroke).max(1.0);
        let rects: &[([f32; 2], [f32; 2])] = match style {
            CursorStyle::Block => return true,
            CursorStyle::Bar => &[([left, top], [thick, height])],
            CursorStyle::Underline => &[([left, top + height - thick], [width, thick])],
            CursorStyle::Hollow => &[
                ([left, top], [width, stroke]),
                ([left, top + height - stroke], [width, stroke]),
                ([left, top], [stroke, height]),
                ([left + width - stroke, top], [stroke, height]),
            ],
        };
        for &(origin, size) in rects {
            self.overlays.push(Rect::solid(origin, size, color));
        }
        false
    }

    /// Adds a line of the window's own text. Drawn after the grid's
    /// glyphs, so it sits on top of them: these glyphs go to a list of
    /// their own, drawn last.
    #[allow(clippy::cast_precision_loss)]
    fn push_text(&mut self, text: &Text<'_>, fonts: &mut Fonts) {
        let cell = fonts.cell;
        let (cell_width, cell_height) = (cell.width as f32, cell.height as f32);
        let bold = matches!(text.ink, Ink::OnColor | Ink::Bold | Ink::Key);
        let color = self.ink(text.ink);
        let [mut pen_x, top] = text.origin;
        match text.font {
            TextFont::Mono => {
                let variant = if bold {
                    Variant::Bold
                } else {
                    Variant::Regular
                };
                let baseline = top + cell.baseline;
                for ch in text.content.chars() {
                    let width = mono_cells(ch) as f32 * cell_width;
                    if let Some((face, glyph)) = fonts.glyph(ch, variant)
                        && let Some(atlas) = self.glyphs.get(fonts, face, glyph)
                    {
                        self.text_glyphs.push(place(
                            &atlas,
                            [pen_x, baseline],
                            [width, cell_height],
                            color,
                            cell.baseline,
                        ));
                    }
                    pen_x += width;
                }
            }
            TextFont::Ui => {
                let ui = fonts.ui;
                let baseline = top + ui.baseline;
                for ch in text.content.chars() {
                    let Some(glyph) = fonts.ui_glyph(ch, bold) else {
                        continue;
                    };
                    if let Some(atlas) =
                        self.glyphs
                            .get_sized(fonts, glyph.face, glyph.glyph, glyph.ppem)
                    {
                        // Whole pixels, so glyphs stay sharp.
                        self.text_glyphs.push(place(
                            &atlas,
                            [pen_x.round(), baseline],
                            [glyph.advance, ui.height],
                            color,
                            ui.baseline,
                        ));
                    }
                    pen_x += glyph.advance.round();
                }
            }
        }
    }

    /// Adds the glyphs for one cell's text: its character, or the glyphs
    /// its codepoints are shaped into, as a joined emoji or a letter and
    /// its accents.
    #[allow(clippy::too_many_arguments)]
    fn push_cell_text(
        &mut self,
        fonts: &mut Fonts,
        row: &clackatoo_term::Row,
        col: usize,
        cell: Cell,
        variant: Variant,
        pen: [f32; 2],
        cell_box: [f32; 2],
        color: Rgba,
    ) {
        let base = cell.ch();
        if base == ' ' {
            return;
        }
        if boxdraw::is_drawn(base) && !cell.has_grapheme() {
            if let Some(atlas) = self.glyphs.drawn(base, &fonts.cell) {
                self.glyph_instances
                    .push(place(&atlas, pen, cell_box, color, fonts.cell.baseline));
            }
            return;
        }
        if !cell.has_grapheme() {
            if let Some(atlas) = self.char_glyph(fonts, base, variant) {
                self.glyph_instances
                    .push(place(&atlas, pen, cell_box, color, fonts.cell.baseline));
            }
            return;
        }
        let mut text = std::mem::take(&mut self.cell_text);
        text.clear();
        row.push_text(col, &mut text);
        self.shape(fonts, &text, variant);
        let shaped = &self.shaped[variant as usize];
        if let Some(glyphs) = shaped.get(text.as_str()).and_then(Option::as_deref) {
            // An emoji fills the cell, so one is drawn: what a sequence
            // joins into, or its first if the font can't join it.
            let emoji = glyphs
                .iter()
                .find(|(atlas, _)| atlas.color)
                .map(std::slice::from_ref);
            for (atlas, offset) in emoji.unwrap_or(glyphs) {
                let pen = [pen[0] + offset[0], pen[1] + offset[1]];
                self.glyph_instances
                    .push(place(atlas, pen, cell_box, color, fonts.cell.baseline));
            }
            self.cell_text = text;
            return;
        }
        // Each codepoint from whichever font has it.
        for (index, ch) in text.chars().enumerate() {
            let Some(atlas) = self.char_glyph(fonts, ch, variant) else {
                continue;
            };
            if atlas.color && index > 0 {
                // A joined emoji's later parts, which need shaping to draw.
                break;
            }
            // Combining marks are drawn from where the character before them
            // ends, as text shaping would place them.
            let pen = if index == 0 {
                pen
            } else {
                [pen[0] + cell_box[0], pen[1]]
            };
            self.glyph_instances
                .push(place(&atlas, pen, cell_box, color, fonts.cell.baseline));
            if atlas.color {
                break;
            }
        }
        self.cell_text = text;
    }

    /// Shapes `text` in `variant` into `self.shaped`, unless it's there
    /// already: shaping a text is slow, so each is shaped once, then drawn
    /// from the cache every frame.
    fn shape(&mut self, fonts: &mut Fonts, text: &str, variant: Variant) {
        let shaped = &mut self.shaped[variant as usize];
        if shaped.contains_key(text) {
            return;
        }
        let glyphs = fonts.shape(text, variant).map(|glyphs| {
            glyphs
                .iter()
                .filter_map(|shaped| {
                    let atlas = self.glyphs.get(fonts, shaped.face, shaped.glyph)?;
                    Some((atlas, shaped.offset))
                })
                .collect()
        });
        shaped.insert(text.into(), glyphs);
    }

    /// Draws the cells `cols` of `row` as one emoji two cells wide at `pen`,
    /// in `color` unless it has colours of its own, and returns whether the
    /// font joined them into one glyph to draw.
    fn push_joined_text(
        &mut self,
        fonts: &mut Fonts,
        row: &clackatoo_term::Row,
        cols: std::ops::Range<usize>,
        variant: Variant,
        pen: [f32; 2],
        color: Rgba,
    ) -> bool {
        let mut text = std::mem::take(&mut self.cell_text);
        text.clear();
        for col in cols {
            row.push_text(col, &mut text);
        }
        self.shape(fonts, &text, variant);
        let joined = match self.shaped[variant as usize].get(text.as_str()) {
            Some(Some(glyphs)) if glyphs.len() == 1 => Some(glyphs[0]),
            _ => None,
        };
        self.cell_text = text;
        let Some((atlas, offset)) = joined else {
            return false;
        };
        let cell = fonts.cell;
        #[allow(clippy::cast_precision_loss)]
        let cell_box = [2.0 * cell.width as f32, cell.height as f32];
        let pen = [pen[0] + offset[0], pen[1] + offset[1]];
        self.glyph_instances
            .push(place(&atlas, pen, cell_box, color, cell.baseline));
        true
    }
}

/// The first character that can start a run of cells drawn as one: ☝,
/// the first emoji a skin tone goes on. Checked before anything else, so
/// plain text costs one comparison.
const FIRST_JOINING: char = '\u{261D}';

/// Where a run of cells from `col` that's drawn as one glyph ends, if the
/// cell at `col` starts one. The grid gives each character with a width
/// its own cells, as programs count them, so some emoji are split across
/// cells: a flag is two regional indicators of a cell each, and a skin
/// tone is a wide character of its own after the emoji it colours. Drawn
/// one per cell they'd show as two letters, or as a hand and a swatch.
///
/// This is where text is gathered from several cells to be shaped as one,
/// so ligatures would make their runs here too. Only cells up to `cols`,
/// those shown, are joined.
fn joined_run(row: &clackatoo_term::Row, col: usize, cols: usize) -> Option<usize> {
    let cells = row.cells();
    let cell = cells[col];
    if cell.ch() < FIRST_JOINING {
        return None;
    }
    let next = col + if cell.is_wide() { 2 } else { 1 };
    let next_cell = *cells.get(next).filter(|_| next < cols)?;
    let next_width = if next_cell.is_wide() { 2 } else { 1 };
    let joins = if is_regional_indicator(cell.ch()) {
        // A flag is a pair, counted from the left: the cell to the left
        // has drawn it if it was the first of a pair. Each indicator
        // stands alone if anything was added to it.
        is_regional_indicator(next_cell.ch()) && !cell.has_grapheme() && !next_cell.has_grapheme()
    } else {
        is_skin_tone(next_cell.ch()) && !is_skin_tone(cell.ch())
    };
    (joins && next + next_width <= cols).then_some(next + next_width)
}

/// Whether `row`, if it wraps onto the next, ends halfway through a flag:
/// with an odd number of regional indicators running to its end, counted
/// from where the run starts. That's on a row before if the run fills
/// `row`, and `continued` says whether that row ended halfway through one.
fn ends_mid_flag(row: &clackatoo_term::Row, continued: bool) -> bool {
    if !row.is_wrapped() {
        return false;
    }
    let cells = row.cells();
    let run = (cells.iter().rev())
        .take_while(|cell| is_regional_indicator(cell.ch()) && !cell.has_grapheme())
        .count();
    let before = if run == cells.len() {
        usize::from(continued)
    } else {
        0
    };
    (before + run) % 2 == 1
}

/// Copies what's drawn in `texture` back from the GPU, once the GPU has
/// drawn it, and returns its pixels, row by row from the top, four bytes
/// each, in the texture's format. For drawing offscreen: tests, and
/// looking at the window without a desktop.
pub(crate) fn read_back(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    texture: &wgpu::Texture,
) -> Vec<u8> {
    let [width, height] = [texture.width(), texture.height()];
    // Rows of a texture copy must be padded to 256 bytes.
    let row_bytes = width * 4;
    let padded = row_bytes.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded * height),
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
                rows_per_image: Some(height),
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
    // A row at a time: byte by byte is slow without optimisations, and a
    // big window's frame is millions of them.
    let mut pixels = Vec::with_capacity((row_bytes * height) as usize);
    for row in mapped.chunks(padded as usize) {
        pixels.extend_from_slice(&row[..row_bytes as usize]);
    }
    pixels
}

/// The uniforms, the atlas and its sampler.
fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("terminal"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// A pipeline drawing instanced quads, each instance laid out by
/// `(attributes, stride)`, with premultiplied alpha blending.
fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &str,
    (vertex, fragment): (&str, &str),
    (attributes, stride): (&[wgpu::VertexAttribute], u64),
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: stride,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Where a glyph goes for a cell whose pen (left edge, baseline) is at
/// `pen`. Coloured glyphs, emoji, are scaled to fit the cell box and
/// centred in it, since their fonts rarely match the cell size.
#[allow(clippy::cast_precision_loss)]
fn place(
    atlas: &AtlasGlyph,
    pen: [f32; 2],
    cell_box: [f32; 2],
    color: Rgba,
    baseline: f32,
) -> Glyph {
    let [x, y, width, height] = atlas.rect.map(|value| value as f32);
    let (origin, size) = if atlas.color {
        let scale = (cell_box[0] / width).min(cell_box[1] / height);
        let size = [width * scale, height * scale];
        let top = pen[1] - baseline;
        let origin = [
            pen[0] + (cell_box[0] - size[0]) / 2.0,
            top + (cell_box[1] - size[1]) / 2.0,
        ];
        (origin, size)
    } else {
        let origin = [pen[0] + atlas.left as f32, pen[1] - atlas.top as f32];
        (origin, [width, height])
    };
    Glyph {
        origin,
        size,
        uv_origin: [x, y],
        uv_size: [width, height],
        color,
        colored: u32::from(atlas.color),
        clip: NO_CLIP,
    }
}

/// A vertex buffer of instances that grows as needed.
#[derive(Debug)]
struct InstanceBuffer {
    label: &'static str,
    buffer: Option<wgpu::Buffer>,
}

impl InstanceBuffer {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            buffer: None,
        }
    }

    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let len = bytes.len() as u64;
        if self
            .buffer
            .as_ref()
            .is_none_or(|buffer| buffer.size() < len)
        {
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: len.next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buffer = self.buffer.as_ref().expect("the buffer was just made");
        queue.write_buffer(buffer, 0, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(shape: Underline, left: f32, width: f32) -> Vec<([f32; 2], [f32; 2])> {
        let mut rects = Vec::new();
        underline(shape, [left, 10.0], width, 2.0, |origin, size| {
            rects.push((origin, size));
        });
        rects
    }

    #[test]
    fn underlines_take_their_shapes() {
        assert_eq!(drawn(Underline::None, 0.0, 20.0), []);
        assert_eq!(
            drawn(Underline::Single, 0.0, 20.0),
            [([0.0, 10.0], [20.0, 2.0])]
        );
        assert_eq!(drawn(Underline::Double, 0.0, 20.0).len(), 2);
        // Dots every 4 pixels, placed by the window, so the next cell's
        // carry on where these stop.
        let dots: Vec<f32> = (drawn(Underline::Dotted, 10.0, 10.0).iter())
            .map(|(origin, _)| origin[0])
            .collect();
        assert_eq!(dots, [12.0, 16.0]);
        // A wave a pixel column at a time, between the top and twice the
        // stroke below it, and with no gaps between columns.
        let wave = drawn(Underline::Curly, 0.0, 24.0);
        assert_eq!(wave.len(), 24);
        for pair in wave.windows(2) {
            let ((a, a_size), (b, b_size)) = (pair[0], pair[1]);
            assert!(
                a[1] <= b[1] + b_size[1] && b[1] <= a[1] + a_size[1],
                "{pair:?}"
            );
        }
        let lowest = wave.iter().map(|(origin, size)| origin[1] + size[1]);
        assert!(lowest.fold(0.0, f32::max) <= 10.0 + 4.0 + 2.0);
    }

    fn row(text: &str) -> clackatoo_term::Row {
        let mut builder = clackatoo_term::RowBuilder::new(20);
        builder.push(text, &clackatoo_term::Style::DEFAULT, false);
        builder.finish(false)
    }

    #[test]
    fn flags_and_skin_tones_are_runs_of_cells() {
        // Each regional indicator is a cell, and they pair from the left.
        let flags = row("🇬🇧🇯🇵🇺 a");
        assert_eq!(flags.text(), "🇬🇧🇯🇵🇺 a");
        assert_eq!(joined_run(&flags, 0, 20), Some(2));
        assert_eq!(joined_run(&flags, 2, 20), Some(4));
        assert_eq!(joined_run(&flags, 4, 20), None, "the odd one out");
        assert_eq!(joined_run(&flags, 6, 20), None, "plain text");
        // A skin tone is two cells of its own, after its emoji's two.
        let tones = row("👍🏽👩🏽\u{200D}💻🏽");
        assert_eq!(joined_run(&tones, 0, 20), Some(4));
        // The rest of a sequence joins the tone's cell.
        assert_eq!(joined_run(&tones, 4, 20), Some(8));
        assert_eq!(joined_run(&tones, 6, 20), None);
        assert_eq!(joined_run(&tones, 8, 20), None, "a tone on its own");
        // Nothing past the cells shown is joined.
        assert_eq!(joined_run(&flags, 0, 1), None);
        assert_eq!(joined_run(&tones, 0, 3), None);
        assert_eq!(joined_run(&tones, 0, 4), Some(4));
        // An indicator with something added to it stands alone.
        let marked = row("🇬\u{301}🇧");
        assert_eq!(joined_run(&marked, 0, 20), None);
    }

    #[test]
    fn flags_and_skin_tones_are_drawn_as_one_glyph_shaped_once() {
        let Some((device, _)) = crate::tests::gpu() else {
            eprintln!("no GPU available; skipping");
            return;
        };
        let mut fonts = crate::tests::fonts(1.0);
        if fonts.glyph('👍', Variant::Regular).is_none() {
            eprintln!("no emoji font; skipping");
            return;
        }
        let row = row("🇬🇧 👍🏽 👩🏽\u{200D}💻 🇬 é");
        let rows = [&row];
        let mut renderer = Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let drawn = |renderer: &mut Renderer, fonts: &mut Fonts| {
            let picture = Picture {
                scenes: &[Scene {
                    origin: [0.0; 2],
                    #[allow(clippy::cast_precision_loss)]
                    size: [20.0 * fonts.cell.width as f32, fonts.cell.height as f32],
                    rows: &rows,
                    cursor: None,
                    normal_mode: false,
                    highlights: &[],
                }],
                fills: &[],
                over: &[],
                texts: &[],
                glyphs: &[],
                over_glyphs: &[],
                images: &[],
                transparent: false,
            };
            renderer.build(&picture, fonts);
            (renderer.glyph_instances.iter())
                .map(|glyph| (glyph.origin[0], glyph.colored))
                .collect::<Vec<_>>()
        };
        let first = drawn(&mut renderer, &mut fonts);
        // The flag, the hand, the technologist, the lone letter, and é.
        assert_eq!(first.len(), 5, "{first:?}");
        assert!(first[..4].iter().all(|&(_, colored)| colored == 1));
        // Each joined emoji starts at its first cell.
        #[allow(clippy::cast_precision_loss)]
        let cell = fonts.cell.width as f32;
        let starts: Vec<f32> = first.iter().map(|&(x, _)| (x / cell).floor()).collect();
        assert_eq!(starts[..4], [0.0, 3.0, 8.0, 13.0]);
        let shaped: Vec<&str> = renderer.shaped[Variant::Regular as usize]
            .keys()
            .map(AsRef::as_ref)
            .collect();
        for text in ["🇬🇧", "👍🏽", "👩🏽\u{200D}💻"] {
            assert!(shaped.contains(&text), "{text} in {shaped:?}");
        }
        // The next frame draws the same, from the cache.
        let cached = renderer.shaped[Variant::Regular as usize].len();
        assert_eq!(drawn(&mut renderer, &mut fonts), first);
        assert_eq!(renderer.shaped[Variant::Regular as usize].len(), cached);
    }

    /// A row `width` cells wide holding `text`, continuing on the next if
    /// `wrapped`.
    fn row_of(width: usize, text: &str, wrapped: bool) -> clackatoo_term::Row {
        let mut builder = clackatoo_term::RowBuilder::new(width);
        builder.push(text, &clackatoo_term::Style::DEFAULT, false);
        builder.finish(wrapped)
    }

    #[test]
    fn a_wrapped_row_can_end_halfway_through_a_flag() {
        assert!(ends_mid_flag(&row_of(3, "ab🇬", true), false));
        assert!(!ends_mid_flag(&row_of(3, "a🇬🇧", true), false));
        assert!(!ends_mid_flag(&row_of(3, "ab🇬", false), false), "no wrap");
        // A run filling the row counts on from the row before.
        assert!(ends_mid_flag(&row_of(3, "🇬🇧🇯", true), false));
        assert!(!ends_mid_flag(&row_of(3, "🇬🇧🇯", true), true));
        assert!(ends_mid_flag(&row_of(2, "🇬🇧", true), true));
    }

    #[test]
    fn flags_pair_from_the_left_whether_or_not_they_join() {
        let Some((device, _)) = crate::tests::gpu() else {
            eprintln!("no GPU available; skipping");
            return;
        };
        let mut fonts = crate::tests::fonts(1.0);
        if fonts.glyph('🇬', Variant::Regular).is_none() {
            eprintln!("no emoji font; skipping");
            return;
        }
        let mut renderer = Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        // As if the font couldn't join 🇽🇬 or 🇦🇦: Noto Color Emoji draws
        // each as one glyph anyway.
        for pair in ["🇽🇬", "🇦🇦"] {
            renderer.shaped[Variant::Regular as usize].insert(pair.into(), None);
        }
        // Each glyph drawn, by its line and column, and whether it's two
        // cells wide, as a flag is, or one, as a letter alone is.
        let mut drawn = |rows: &[&clackatoo_term::Row]| {
            #[allow(clippy::cast_precision_loss)]
            let cell = [fonts.cell.width as f32, fonts.cell.height as f32];
            let picture = Picture {
                scenes: &[Scene {
                    origin: [0.0; 2],
                    #[allow(clippy::cast_precision_loss)]
                    size: [
                        rows[0].width() as f32 * cell[0],
                        rows.len() as f32 * cell[1],
                    ],
                    rows,
                    cursor: None,
                    normal_mode: false,
                    highlights: &[],
                }],
                fills: &[],
                over: &[],
                texts: &[],
                glyphs: &[],
                over_glyphs: &[],
                images: &[],
                transparent: false,
            };
            renderer.build(&picture, &mut fonts);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            (renderer.glyph_instances.iter())
                .map(|glyph| {
                    let line = (glyph.origin[1] / cell[1]).floor().max(0.0) as usize;
                    let col = (glyph.origin[0] / cell[0]).floor() as usize;
                    (line, col, glyph.size[0] > 1.5 * cell[0])
                })
                .collect::<Vec<_>>()
        };
        // 🇽🇬 and 🇦🇦 aren't flags, so their letters stand alone, and the
        // second doesn't pair with the next: 🇬🇧 is drawn, not 🇦🇬.
        let row = row_of(9, "🇽🇬🇧 🇦🇦🇬🇧", false);
        assert_eq!(
            drawn(&[&row]),
            [
                (0, 0, false),
                (0, 1, false),
                (0, 2, false),
                (0, 4, false),
                (0, 5, false),
                (0, 6, true),
            ]
        );
        // A flag the wrap cut in two: its second half stands alone, and
        // the pair after it is 🇯🇵, not 🇧🇯.
        let first = row_of(3, "ab🇬", true);
        let second = row_of(3, "🇧🇯🇵", false);
        let glyphs = drawn(&[&first, &second]);
        // After a and b.
        assert_eq!(
            glyphs[2..],
            [(0, 2, false), (1, 0, false), (1, 1, true)],
            "{glyphs:?}"
        );
    }
}
