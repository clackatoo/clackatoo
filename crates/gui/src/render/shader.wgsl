// Draws the window: rectangles, square or rounded and perhaps bordered
// (backgrounds, the cursor, underlines, cards, bars), and glyphs from the
// atlas. Every shape is an instanced quad, positioned
// in physical pixels from the top left. Colours are premultiplied. Each
// shape is cut off outside its clip box (left, top, right, bottom), as a
// chat pane's are at its edges.

struct Uniforms {
    viewport: vec2<f32>,
    atlas_size: vec2<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

// The corner of a quad drawn as a four-vertex triangle strip.
fn corner(index: u32) -> vec2<f32> {
    return vec2<f32>(f32(index & 1u), f32(index >> 1u));
}

fn to_clip(pixels: vec2<f32>) -> vec4<f32> {
    let ndc = pixels / uniforms.viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    return vec4<f32>(ndc, 0.0, 1.0);
}

struct RectOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Where this fragment is, from the rectangle's centre, in pixels.
    @location(1) offset: vec2<f32>,
    @location(2) @interpolate(flat) half_size: vec2<f32>,
    @location(3) @interpolate(flat) radius: f32,
    @location(4) @interpolate(flat) border: f32,
    @location(5) border_color: vec4<f32>,
    @location(6) @interpolate(flat) clip: vec4<f32>,
}

// Whether a fragment at `position` falls outside `clip`.
fn clipped(position: vec4<f32>, clip: vec4<f32>) -> bool {
    return position.x < clip.x || position.y < clip.y || position.x >= clip.z
        || position.y >= clip.w;
}

@vertex
fn rect_vs(
    @builtin(vertex_index) vertex: u32,
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) radius: f32,
    @location(4) border: f32,
    @location(5) border_color: vec4<f32>,
    @location(6) clip: vec4<f32>,
) -> RectOut {
    let at = corner(vertex);
    var out: RectOut;
    out.position = to_clip(origin + at * size);
    out.color = vec4<f32>(color.rgb * color.a, color.a);
    out.offset = (at - vec2<f32>(0.5)) * size;
    out.half_size = size / 2.0;
    out.radius = radius;
    out.border = border;
    out.border_color = vec4<f32>(border_color.rgb * border_color.a, border_color.a);
    out.clip = clip;
    return out;
}

// How far `offset` is outside a box with rounded corners: negative inside.
fn rounded_box(offset: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(offset) - half_size + vec2<f32>(radius);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

@fragment
fn rect_fs(in: RectOut) -> @location(0) vec4<f32> {
    if clipped(in.position, in.clip) {
        discard;
    }
    // Cells, underlines and the cursor: square and solid.
    if in.radius <= 0.0 && in.border <= 0.0 {
        return in.color;
    }
    var color = in.color;
    let distance = rounded_box(in.offset, in.half_size, in.radius);
    // A pixel's worth of soft edge, so curves aren't jagged.
    let coverage = clamp(0.5 - distance, 0.0, 1.0);
    if in.border > 0.0 {
        let inside_border = clamp(0.5 - (distance + in.border), 0.0, 1.0);
        color = mix(in.border_color, color, inside_border);
    }
    return color * coverage;
}

struct GlyphOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) colored: u32,
    @location(3) @interpolate(flat) clip: vec4<f32>,
}

@vertex
fn glyph_vs(
    @builtin(vertex_index) vertex: u32,
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_origin: vec2<f32>,
    @location(3) uv_size: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) colored: u32,
    @location(6) clip: vec4<f32>,
) -> GlyphOut {
    let at = corner(vertex);
    var out: GlyphOut;
    out.position = to_clip(origin + at * size);
    out.uv = (uv_origin + at * uv_size) / uniforms.atlas_size;
    out.color = vec4<f32>(color.rgb * color.a, color.a);
    out.colored = colored;
    out.clip = clip;
    return out;
}

@fragment
fn glyph_fs(in: GlyphOut) -> @location(0) vec4<f32> {
    if clipped(in.position, in.clip) {
        discard;
    }
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    if in.colored == 1u {
        return texel;
    }
    // A shape: the atlas holds its coverage, and the text colour fills it.
    return in.color * texel.a;
}
