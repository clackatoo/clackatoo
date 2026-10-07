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

use bytemuck::{Pod, Zeroable};
use clackatoo_protocol::{Attributes, Underline};
use clackatoo_term::Cell;

use crate::boxdraw;
use crate::colors::{Rgba, Theme};
use crate::fonts::{Fonts, Variant};
use crate::glyphs::{ATLAS_SIZE, AtlasGlyph, GlyphCache};

/// Everything one frame shows.
#[derive(Debug)]
pub(crate) struct Picture<'a> {
    /// The panes.
    pub(crate) scenes: &'a [Scene<'a>],
    /// Rectangles of the window's own, such as the cards and the names in
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
}

/// A rectangle of the window's own: perhaps rounded, perhaps bordered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Fill {
    /// Left, top, width and height, in pixels.
    pub(crate) rect: [f32; 4],
    pub(crate) paint: Paint,
    /// How round its corners are, in pixels.
    pub(crate) radius: f32,
    /// A border inside its edge: how thick, and its colour.
    pub(crate) border: Option<(f32, Paint)>,
    /// A fade to another colour: the colour, and which way, as a vector
    /// across the rectangle, where one is its whole width or height.
    pub(crate) fade: Option<(Paint, [f32; 2])>,
}

impl Fill {
    /// A square-cornered, borderless rectangle.
    pub(crate) fn solid(rect: [f32; 4], paint: Paint) -> Self {
        Self {
            rect,
            paint,
            radius: 0.0,
            border: None,
            fade: None,
        }
    }

    /// A rectangle with corners `radius` round.
    pub(crate) fn rounded(rect: [f32; 4], paint: Paint, radius: f32) -> Self {
        Self {
            rect,
            paint,
            radius,
            border: None,
            fade: None,
        }
    }

    /// With a border `width` thick, in `paint`.
    pub(crate) fn bordered(self, width: f32, paint: Paint) -> Self {
        Self {
            border: Some((width, paint)),
            ..self
        }
    }

    /// Fading to `paint` towards `direction`: `[0.0, 1.0]` top to bottom,
    /// `[1.0, 1.0]` corner to corner.
    pub(crate) fn fading(self, paint: Paint, direction: [f32; 2]) -> Self {
        Self {
            fade: Some((paint, direction)),
            ..self
        }
    }
}

/// The colours the window's own things are drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    /// Nothing: for a border alone.
    Clear,
    /// The window's background, at the bottom left of its fade...
    Backdrop,
    /// ...and at the top right, bluer.
    BackdropEnd,
    /// The faint line round the window.
    WindowEdge,
    /// A soft shadow, under something raised.
    Shadow,
    /// A pane's card, the colour its terminal's background is.
    Card,
    /// A card's edge.
    Edge,
    /// The focused card's edge, while typing goes to its program...
    Focus,
    /// ...and the glow around it.
    FocusGlow,
    /// clackatoo's own colour: the focused card's edge in normal mode...
    Accent,
    /// ...and the glow around it.
    AccentGlow,
    /// Behind what's picked, such as the workspace shown.
    Picked,
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
    /// Bold, in the text colour, for what's picked.
    Strong,
    /// In the warning colour, for what's gone wrong.
    Warning,
}

/// Which font text of the window's own is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextFont {
    /// The terminal's, a cell per character, as for commands being typed.
    Mono,
    /// The interface font, as for names in the bars.
    Ui,
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

/// Why text is highlighted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HighlightKind {
    /// It's selected.
    Selection,
    /// A search found it.
    Match,
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
    /// An outline of the cell, while the window isn't focused.
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
    /// The colour it fades to, along `direction`.
    color_to: Rgba,
    /// Which way it fades from `color` to `color_to`, as a vector across
    /// the rectangle, where one is its whole width or height. Zero for no
    /// fade.
    direction: [f32; 2],
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
            color_to: color,
            direction: [0.0; 2],
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
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct Uniforms {
    viewport: [f32; 2],
    atlas_size: [f32; 2],
}

/// The GPU objects that draw a terminal.
#[derive(Debug)]
pub(crate) struct Renderer {
    rect_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
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
    /// Holds a cell's text when it has more than one codepoint.
    cell_text: String,
}

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
                4 => Float32, 5 => Float32x4, 6 => Float32x4, 7 => Float32x2,
            ],
            std::mem::size_of::<Rect>() as u64,
        );
        let glyph_pipeline = pipeline(
            "glyphs",
            ("glyph_vs", "glyph_fs"),
            &wgpu::vertex_attr_array![
                0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2,
                4 => Float32x4, 5 => Uint32,
            ],
            std::mem::size_of::<Glyph>() as u64,
        );
        Self {
            rect_pipeline,
            glyph_pipeline,
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
            cell_text: String::new(),
        }
    }

    /// Throws away every rasterised glyph, after the text size changed.
    pub(crate) fn forget_glyphs(&mut self) {
        self.glyphs = GlyphCache::new();
        self.char_glyphs = CharGlyphs::new();
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
                self.theme.backdrop
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

    /// Turns the picture into rectangles and glyphs.
    fn build(&mut self, picture: &Picture<'_>, fonts: &mut Fonts) {
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
            let color = self.theme.paint(fill.paint);
            let (color_to, direction) =
                fill.fade.map_or((color, [0.0; 2]), |(paint, direction)| {
                    (self.theme.paint(paint), direction)
                });
            Rect {
                origin: [left, top],
                size: [width, height],
                color,
                radius: fill.radius,
                border,
                border_color,
                color_to,
                direction,
            }
        };
        let (under, over): (Vec<Rect>, Vec<Rect>) = (
            picture.fills.iter().map(rect).collect(),
            picture.over.iter().map(rect).collect(),
        );
        // The cards go under the panes' cells, and the palette over them.
        self.rects.splice(0..0, under);
        self.overlays.extend(over);
        for text in picture.texts {
            self.push_text(text, fonts);
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
        for (line, row) in scene.rows.iter().take(lines).enumerate() {
            let top = origin_y + line as f32 * cell_height;
            let baseline = top + cell.baseline;
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
                let width = cells * cell_width;
                if style.underline != Underline::None {
                    self.overlays.push(Rect::solid(
                        [left, baseline + cell.underline_offset],
                        [width, cell.stroke],
                        colors.underline,
                    ));
                }
                if style.attributes.contains(Attributes::STRIKETHROUGH) {
                    self.overlays.push(Rect::solid(
                        [left, baseline - cell.strikeout_offset - cell.stroke / 2.0],
                        [width, cell.stroke],
                        colors.foreground,
                    ));
                }
            }
        }
        for highlight in scene.highlights {
            if highlight.line >= lines {
                continue;
            }
            let [r, g, b, _] = self.theme.accent;
            // Translucent, so the text's own colours show through.
            let alpha = match highlight.kind {
                HighlightKind::Selection => 0.35,
                HighlightKind::Match => 0.6,
            };
            let start = highlight.cols.start.min(cols);
            let end = highlight.cols.end.min(cols);
            self.rects.push(Rect::solid(
                [
                    origin_x + start as f32 * cell_width,
                    origin_y + highlight.line as f32 * cell_height,
                ],
                [(end - start) as f32 * cell_width, cell_height],
                [r, g, b, alpha],
            ));
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
        let bold = text.ink == Ink::Strong;
        let color = match text.ink {
            Ink::Plain | Ink::Strong => self.theme.foreground,
            Ink::Muted => self.theme.muted,
            Ink::Warning => self.theme.warning,
        };
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
                    if let Some((face, glyph)) = fonts.glyph(ch, variant)
                        && let Some(atlas) = self.glyphs.get(fonts, face, glyph)
                    {
                        self.text_glyphs.push(place(
                            &atlas,
                            [pen_x, baseline],
                            [cell_width, cell_height],
                            color,
                            cell.baseline,
                        ));
                    }
                    pen_x += cell_width;
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

    /// Adds the glyphs for one cell's text: its character, then any
    /// combining marks drawn over it. Emoji sequences show their first
    /// emoji until text shaping arrives.
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
