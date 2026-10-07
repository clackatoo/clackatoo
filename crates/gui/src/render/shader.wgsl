// Draws a terminal: solid rectangles (backgrounds, the cursor, underlines)
// and glyphs from the atlas. Every shape is an instanced quad, positioned
// in physical pixels from the top left. Colours are premultiplied.

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
}

@vertex
fn rect_vs(
    @builtin(vertex_index) vertex: u32,
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> RectOut {
    var out: RectOut;
    out.position = to_clip(origin + corner(vertex) * size);
    out.color = vec4<f32>(color.rgb * color.a, color.a);
    return out;
}

@fragment
fn rect_fs(in: RectOut) -> @location(0) vec4<f32> {
    return in.color;
}

struct GlyphOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) colored: u32,
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
) -> GlyphOut {
    let at = corner(vertex);
    var out: GlyphOut;
    out.position = to_clip(origin + at * size);
    out.uv = (uv_origin + at * uv_size) / uniforms.atlas_size;
    out.color = vec4<f32>(color.rgb * color.a, color.a);
    out.colored = colored;
    return out;
}

@fragment
fn glyph_fs(in: GlyphOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    if in.colored == 1u {
        return texel;
    }
    // A shape: the atlas holds its coverage, and the text colour fills it.
    return in.color * texel.a;
}
