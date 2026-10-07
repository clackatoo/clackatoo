//! Rasterising glyphs into the texture the GPU draws them from.

use std::collections::HashMap;

use swash::GlyphId;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Transform};

use crate::fonts::{FaceId, Fonts};

/// The atlas texture's width and height, in pixels. Room for thousands of
/// glyphs at any normal size.
pub(crate) const ATLAS_SIZE: u32 = 2048;

/// Where a rasterised glyph is in the atlas, and how it sits on the
/// baseline.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AtlasGlyph {
    /// The glyph's rectangle in the atlas: x, y, width, height.
    pub(crate) rect: [u32; 4],
    /// From the pen position to the glyph's left edge.
    pub(crate) left: i32,
    /// From the baseline up to the glyph's top edge.
    pub(crate) top: i32,
    /// The glyph has its own colours (an emoji), rather than being a shape
    /// drawn in the text colour.
    pub(crate) color: bool,
}

/// Pixels waiting to be copied into the atlas texture.
#[derive(Debug)]
pub(crate) struct Upload {
    pub(crate) rect: [u32; 4],
    /// Premultiplied RGBA.
    pub(crate) pixels: Vec<u8>,
}

/// Every glyph drawn so far, rasterised once and kept in the atlas.
pub(crate) struct GlyphCache {
    context: ScaleContext,
    glyphs: HashMap<(FaceId, GlyphId), Option<AtlasGlyph>>,
    shelves: Shelves,
    /// New glyphs not yet in the texture.
    pub(crate) uploads: Vec<Upload>,
}

impl std::fmt::Debug for GlyphCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphCache")
            .field("glyphs", &self.glyphs.len())
            .finish_non_exhaustive()
    }
}

impl GlyphCache {
    pub(crate) fn new() -> Self {
        Self {
            context: ScaleContext::new(),
            glyphs: HashMap::new(),
            shelves: Shelves::default(),
            uploads: Vec::new(),
        }
    }

    /// The glyph's place in the atlas, rasterising it first if needed.
    /// `None` for glyphs with nothing to draw, such as a space.
    pub(crate) fn get(
        &mut self,
        fonts: &Fonts,
        face: FaceId,
        glyph: GlyphId,
    ) -> Option<AtlasGlyph> {
        if let Some(&cached) = self.glyphs.get(&(face, glyph)) {
            return cached;
        }
        let rasterised = self.rasterise(fonts, face, glyph);
        self.glyphs.insert((face, glyph), rasterised);
        rasterised
    }

    fn rasterise(&mut self, fonts: &Fonts, face_id: FaceId, glyph: GlyphId) -> Option<AtlasGlyph> {
        let face = fonts.face(face_id);
        let mut scaler = self
            .context
            .builder(face.font())
            .size(fonts.ppem)
            .hint(true)
            .build();
        let mut render = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ]);
        render.format(Format::Alpha);
        if face.embolden {
            render.embolden(fonts.ppem / 24.0);
        }
        if face.skew {
            render.transform(Some(Transform::skew(
                swash::zeno::Angle::from_degrees(14.0),
                swash::zeno::Angle::ZERO,
            )));
        }
        let image = render.render(&mut scaler, glyph)?;
        let (width, height) = (image.placement.width, image.placement.height);
        if width == 0 || height == 0 {
            return None;
        }
        let Some([x, y]) = self.shelves.allocate(width, height) else {
            log::warn!("the glyph atlas is full; glyph {glyph} is not drawn");
            return None;
        };
        let color = image.content == Content::Color;
        let pixels = match image.content {
            Content::Mask => image.data.iter().flat_map(|&a| [a, a, a, a]).collect(),
            Content::Color => image
                .data
                .chunks_exact(4)
                .flat_map(|p| {
                    let premultiply = |c: u8| {
                        u8::try_from(u16::from(c) * u16::from(p[3]) / 255).expect("fits in u8")
                    };
                    [
                        premultiply(p[0]),
                        premultiply(p[1]),
                        premultiply(p[2]),
                        p[3],
                    ]
                })
                .collect(),
            Content::SubpixelMask => return None,
        };
        let rect = [x, y, width, height];
        self.uploads.push(Upload { rect, pixels });
        Some(AtlasGlyph {
            rect,
            left: image.placement.left,
            top: image.placement.top,
            color,
        })
    }
}

/// A shelf packer: glyphs fill rows ("shelves") left to right, and a new
/// shelf starts below when none has room.
#[derive(Debug, Default)]
struct Shelves {
    shelves: Vec<Shelf>,
    /// Where the next shelf would start.
    bottom: u32,
}

#[derive(Debug)]
struct Shelf {
    y: u32,
    height: u32,
    /// Where the next glyph on this shelf would start.
    x: u32,
}

impl Shelves {
    /// One pixel between glyphs keeps texture filtering from bleeding.
    const PADDING: u32 = 1;

    fn allocate(&mut self, width: u32, height: u32) -> Option<[u32; 2]> {
        let (padded_width, padded_height) = (width + Self::PADDING, height + Self::PADDING);
        // The first shelf tall enough, and not much taller, with room left.
        if let Some(shelf) = self.shelves.iter_mut().find(|shelf| {
            shelf.height >= padded_height
                && shelf.height <= padded_height + padded_height / 4 + 2
                && shelf.x + padded_width <= ATLAS_SIZE
        }) {
            let at = [shelf.x, shelf.y];
            shelf.x += padded_width;
            return Some(at);
        }
        if self.bottom + padded_height > ATLAS_SIZE || padded_width > ATLAS_SIZE {
            return None;
        }
        let at = [0, self.bottom];
        self.shelves.push(Shelf {
            y: self.bottom,
            height: padded_height,
            x: padded_width,
        });
        self.bottom += padded_height;
        Some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_never_overlap() {
        let mut shelves = Shelves::default();
        let mut placed: Vec<[u32; 4]> = Vec::new();
        for i in 0..3000u32 {
            let (w, h) = (5 + i % 23, 8 + i % 17);
            let Some([x, y]) = shelves.allocate(w, h) else {
                break;
            };
            assert!(x + w <= ATLAS_SIZE && y + h <= ATLAS_SIZE);
            for &[px, py, pw, ph] in &placed {
                let apart = x + w <= px || px + pw <= x || y + h <= py || py + ph <= y;
                assert!(apart, "{:?} overlaps {:?}", [x, y, w, h], [px, py, pw, ph]);
            }
            placed.push([x, y, w, h]);
        }
        assert!(placed.len() > 1000);
    }
}
