//! Drawing a terminal's screen with the GPU.
//!
//! Each frame turns the screen into two lists of instanced quads, solid
//! rectangles and glyphs, and draws them in three passes over one render
//! pass: backgrounds and the cursor, then glyphs, then underlines and
//! strikethroughs on top. The renderer draws into any texture, so the same
//! code serves the window and offscreen tests.

use bytemuck::{Pod, Zeroable};
use clackatoo_protocol::{Attributes, Underline};
use clackatoo_term::Cell;

use crate::boxdraw;
use crate::colors::{Rgba, Theme};
use crate::fonts::{Fonts, Variant};
use crate::glyphs::{ATLAS_SIZE, AtlasGlyph, GlyphCache};

/// Everything one frame shows.
#[derive(Debug)]
pub(crate) struct Scene<'a> {
    /// The rows to show, top to bottom.
    pub(crate) rows: &'a [&'a clackatoo_term::Row],
    pub(crate) cursor: Option<SceneCursor>,
    /// Frame the window, to show the pane is in normal mode.
    pub(crate) normal_mode: bool,
    /// A label for the top right corner, such as the scroll position.
    pub(crate) label: Option<&'a str>,
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
    label_glyphs: Vec<Glyph>,
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
            &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
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
            label_glyphs: Vec::new(),
        }
    }

    /// Throws away every rasterised glyph, after the text size changed.
    pub(crate) fn forget_glyphs(&mut self) {
        self.glyphs = GlyphCache::new();
    }

    /// Draws `screen` into `target`, which is `size` physical pixels, with
    /// the grid `padding` pixels in from the top left.
    pub(crate) fn render(
        &mut self,
        gpu: (&wgpu::Device, &wgpu::Queue),
        target: &wgpu::TextureView,
        size: [u32; 2],
        padding: f32,
        scene: &Scene<'_>,
        fonts: &mut Fonts,
    ) {
        let (device, queue) = gpu;
        self.build(scene, fonts, padding, size);
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
        self.glyph_instances.append(&mut self.label_glyphs);
        self.rect_buffer
            .write(device, queue, bytemuck::cast_slice(&self.rects));
        self.glyph_buffer
            .write(device, queue, bytemuck::cast_slice(&self.glyph_instances));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let [r, g, b, a] = self.theme.background.map(f64::from);
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

    /// Turns the scene into rectangles and glyphs.
    #[allow(clippy::cast_precision_loss)]
    fn build(&mut self, scene: &Scene<'_>, fonts: &mut Fonts, padding: f32, size: [u32; 2]) {
        self.rects.clear();
        self.overlays.clear();
        self.glyph_instances.clear();
        self.label_glyphs.clear();
        let cell = fonts.cell;
        let (cell_width, cell_height) = (cell.width as f32, cell.height as f32);
        let cursor = scene.cursor;
        for (line, row) in scene.rows.iter().enumerate() {
            let top = padding + line as f32 * cell_height;
            let baseline = top + cell.baseline;
            for (col, &cell_value) in row.cells().iter().enumerate() {
                if cell_value.is_wide_spacer() {
                    continue;
                }
                let cells = if cell_value.is_wide() { 2.0 } else { 1.0 };
                let left = padding + col as f32 * cell_width;
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
                    self.rects.push(Rect {
                        origin: [left, top],
                        size: [cells * cell_width, cell_height],
                        color: background,
                    });
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
                    self.overlays.push(Rect {
                        origin: [left, baseline + cell.underline_offset],
                        size: [width, cell.stroke],
                        color: colors.underline,
                    });
                }
                if style.attributes.contains(Attributes::STRIKETHROUGH) {
                    self.overlays.push(Rect {
                        origin: [left, baseline - cell.strikeout_offset - cell.stroke / 2.0],
                        size: [width, cell.stroke],
                        color: colors.foreground,
                    });
                }
            }
        }
        if scene.normal_mode {
            self.push_frame(size, padding);
        }
        if let Some(label) = scene.label {
            self.push_label(label, fonts, padding, size);
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
            self.overlays.push(Rect {
                origin,
                size,
                color,
            });
        }
        false
    }

    /// A border around the window in the accent colour, inside the padding.
    #[allow(clippy::cast_precision_loss)]
    fn push_frame(&mut self, size: [u32; 2], padding: f32) {
        let (w, h) = (size[0] as f32, size[1] as f32);
        let t = (padding / 2.0).max(1.0).round();
        for (origin, size) in [
            ([0.0, 0.0], [w, t]),
            ([0.0, h - t], [w, t]),
            ([0.0, 0.0], [t, h]),
            ([w - t, 0.0], [t, h]),
        ] {
            self.overlays.push(Rect {
                origin,
                size,
                color: self.theme.accent,
            });
        }
    }

    /// `text` in the top right corner, dark on the accent colour, over
    /// whatever the grid shows there.
    #[allow(clippy::cast_precision_loss)]
    fn push_label(&mut self, text: &str, fonts: &mut Fonts, padding: f32, size: [u32; 2]) {
        let cell = fonts.cell;
        let (cell_width, cell_height) = (cell.width as f32, cell.height as f32);
        let chars = text.chars().count() as f32;
        // A cell's width of space either side of the text.
        let width = (chars + 2.0) * cell_width;
        let left = (size[0] as f32 - padding - width).max(0.0);
        let top = padding;
        self.overlays.push(Rect {
            origin: [left, top],
            size: [width, cell_height],
            color: self.theme.accent,
        });
        let baseline = top + cell.baseline;
        // Drawn after the grid's glyphs, so it sits on top of them: the
        // overlay pass comes last, and these glyphs go to their own list.
        for (index, ch) in text.chars().enumerate() {
            let Some((face, glyph)) = fonts.glyph(ch, Variant::Bold) else {
                continue;
            };
            let Some(atlas) = self.glyphs.get(fonts, face, glyph) else {
                continue;
            };
            let pen = [left + (index as f32 + 1.0) * cell_width, baseline];
            self.label_glyphs.push(place(
                &atlas,
                pen,
                [cell_width, cell_height],
                self.theme.background,
                cell.baseline,
            ));
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
        let mut text = String::new();
        row.push_text(col, &mut text);
        for (index, ch) in text.chars().enumerate() {
            let Some((face, glyph)) = fonts.glyph(ch, variant) else {
                continue;
            };
            let Some(atlas) = self.glyphs.get(fonts, face, glyph) else {
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
