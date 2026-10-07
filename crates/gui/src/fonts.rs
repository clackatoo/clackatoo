//! Finding fonts, measuring cells, and turning characters into glyphs.
//!
//! The configured family supplies the regular, bold, italic and bold italic
//! faces. A character none of them has is looked up in fallback fonts
//! (CJK, emoji, other scripts), found through fontconfig and cached.

use std::collections::HashMap;

use anyhow::Context;
use fontique::{
    Attributes, Blob, Collection, CollectionOptions, FallbackKey, FontStyle, FontWeight,
    GenericFamily, QueryFamily, QueryStatus, Script, SourceCache,
};
use swash::text::Codepoint;
use swash::{CacheKey, FontRef, GlyphId};

/// What to draw text with.
#[derive(Clone, Debug)]
pub struct FontOptions {
    /// The font family. `None` uses the system's monospace font.
    pub family: Option<String>,
    /// The size in points.
    pub size: f32,
}

impl Default for FontOptions {
    fn default() -> Self {
        Self {
            family: None,
            size: 12.0,
        }
    }
}

/// Which of the primary faces a cell is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Variant {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl Variant {
    pub(crate) fn new(bold: bool, italic: bool) -> Self {
        match (bold, italic) {
            (false, false) => Self::Regular,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (true, true) => Self::BoldItalic,
        }
    }

    fn attributes(self) -> Attributes {
        let (weight, style) = match self {
            Self::Regular => (FontWeight::NORMAL, FontStyle::Normal),
            Self::Bold => (FontWeight::BOLD, FontStyle::Normal),
            Self::Italic => (FontWeight::NORMAL, FontStyle::Italic),
            Self::BoldItalic => (FontWeight::BOLD, FontStyle::Italic),
        };
        Attributes::new(fontique::FontWidth::NORMAL, style, weight)
    }
}

/// Names a loaded face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FaceId(u16);

/// One loaded font face.
#[derive(Debug)]
pub(crate) struct Face {
    blob: Blob<u8>,
    /// Where the face's tables start in `blob`, for swash.
    offset: u32,
    /// The face's index in its file, for fontconfig.
    offset_index: u32,
    key: CacheKey,
    /// Draw thicker, because the family has no bold face.
    pub(crate) embolden: bool,
    /// Draw slanted, because the family has no italic face.
    pub(crate) skew: bool,
}

impl Face {
    pub(crate) fn font(&self) -> FontRef<'_> {
        FontRef {
            data: self.blob.as_ref(),
            offset: self.offset,
            key: self.key,
        }
    }

    fn glyph(&self, ch: char) -> Option<GlyphId> {
        Some(self.font().charmap().map(ch)).filter(|&glyph| glyph != 0)
    }
}

/// The size of a cell, and where text sits in it, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellMetrics {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// From the top of the cell down to the baseline.
    pub(crate) baseline: f32,
    /// From the baseline down to the top of an underline.
    pub(crate) underline_offset: f32,
    /// From the baseline up to the middle of a strikethrough.
    pub(crate) strikeout_offset: f32,
    pub(crate) stroke: f32,
}

/// The fonts a window draws with, at one pixel size.
pub(crate) struct Fonts {
    collection: Collection,
    source_cache: SourceCache,
    faces: Vec<Face>,
    /// The four primary faces, by variant, as indices into `faces`.
    primary: [FaceId; 4],
    glyphs: HashMap<(char, Variant), Option<(FaceId, GlyphId)>>,
    /// The configured size, in points.
    points: f32,
    /// The size glyphs are drawn at, in pixels per em.
    pub(crate) ppem: f32,
    pub(crate) cell: CellMetrics,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fonts")
            .field("faces", &self.faces.len())
            .field("ppem", &self.ppem)
            .field("cell", &self.cell)
            .finish_non_exhaustive()
    }
}

impl Fonts {
    /// Loads the configured family at `options.size` points, on a display
    /// with `scale` physical pixels per logical pixel.
    pub(crate) fn load(options: &FontOptions, scale: f64) -> anyhow::Result<Self> {
        let mut collection = Collection::new(CollectionOptions::default());
        let mut source_cache = SourceCache::default();
        let family = match &options.family {
            Some(name) => QueryFamily::Named(name),
            None => QueryFamily::Generic(GenericFamily::Monospace),
        };
        let mut faces = Vec::new();
        let mut primary = [FaceId(0); 4];
        for (slot, variant) in [
            Variant::Regular,
            Variant::Bold,
            Variant::Italic,
            Variant::BoldItalic,
        ]
        .into_iter()
        .enumerate()
        {
            let face = query(&mut collection, &mut source_cache, family, variant)
                .with_context(|| format!("no font found for {family:?}"))?;
            primary[slot] = push_face(&mut faces, face);
        }

        let mut fonts = Self {
            collection,
            source_cache,
            faces,
            primary,
            glyphs: HashMap::new(),
            points: options.size,
            ppem: 0.0,
            cell: CellMetrics {
                width: 1,
                height: 1,
                baseline: 0.0,
                underline_offset: 0.0,
                strikeout_offset: 0.0,
                stroke: 1.0,
            },
        };
        fonts.set_scale(scale);
        Ok(fonts)
    }

    /// Sizes text for a display with `scale` physical pixels per logical
    /// pixel. Glyphs rasterised at the old size must be thrown away.
    pub(crate) fn set_scale(&mut self, scale: f64) {
        // Points are 1/72 inch, and a logical pixel is 1/96 inch.
        #[allow(clippy::cast_possible_truncation)]
        let ppem = (f64::from(self.points) * 96.0 / 72.0 * scale) as f32;
        self.ppem = ppem;
        self.cell = measure(self.face(self.primary[0]), ppem);
    }

    pub(crate) fn face(&self, id: FaceId) -> &Face {
        &self.faces[usize::from(id.0)]
    }

    /// The face and glyph that draw `ch` in `variant`, or `None` if no font
    /// has it.
    pub(crate) fn glyph(&mut self, ch: char, variant: Variant) -> Option<(FaceId, GlyphId)> {
        if let Some(&found) = self.glyphs.get(&(ch, variant)) {
            return found;
        }
        let primary = self.primary[variant as usize];
        let found = match self.face(primary).glyph(ch) {
            Some(glyph) => Some((primary, glyph)),
            None => self.fallback(ch, variant),
        };
        self.glyphs.insert((ch, variant), found);
        found
    }

    /// Searches fallback fonts for `ch`: those fontconfig suggests for its
    /// script, then emoji fonts, then any sans-serif font.
    fn fallback(&mut self, ch: char, variant: Variant) -> Option<(FaceId, GlyphId)> {
        let tag = ch.script().to_opentype().to_be_bytes();
        // OpenType script tags are ISO 15924 codes in lower case.
        let script = Script::from_bytes([tag[0].to_ascii_uppercase(), tag[1], tag[2], tag[3]]);
        let mut found = None;
        let mut query = self.collection.query(&mut self.source_cache);
        query.set_families([
            QueryFamily::Generic(GenericFamily::Emoji),
            QueryFamily::Generic(GenericFamily::SansSerif),
        ]);
        query.set_attributes(variant.attributes());
        query.set_fallbacks(FallbackKey::new(script, None));
        query.matches_with(|font| {
            if font
                .charmap()
                .is_some_and(|charmap| charmap.map(ch).is_some())
            {
                found = Some(font.clone());
                QueryStatus::Stop
            } else {
                QueryStatus::Continue
            }
        });
        drop(query);
        let font = found?;
        // A fallback font found before is reused, not loaded again.
        let id =
            match self.faces.iter().position(|face| {
                face.blob.id() == font.blob.id() && face.offset_index == font.index
            }) {
                Some(index) => FaceId(u16::try_from(index).ok()?),
                None => push_face(&mut self.faces, Face::from_query(&font)?),
            };
        Some((id, self.face(id).glyph(ch)?))
    }
}

impl Face {
    fn from_query(font: &fontique::QueryFont) -> Option<Self> {
        let data = font.blob.as_ref();
        let font_ref = FontRef::from_index(data, usize::try_from(font.index).ok()?)?;
        Some(Self {
            blob: font.blob.clone(),
            offset: font_ref.offset,
            key: font_ref.key,
            offset_index: font.index,
            embolden: font.synthesis.embolden(),
            skew: font.synthesis.skew().is_some(),
        })
    }
}

/// The best match for `family` in `variant`.
fn query(
    collection: &mut Collection,
    source_cache: &mut SourceCache,
    family: QueryFamily<'_>,
    variant: Variant,
) -> Option<Face> {
    let mut found = None;
    let mut query = collection.query(source_cache);
    query.set_families([family]);
    query.set_attributes(variant.attributes());
    query.matches_with(|font| {
        found = Face::from_query(font);
        if found.is_some() {
            QueryStatus::Stop
        } else {
            QueryStatus::Continue
        }
    });
    found
}

fn push_face(faces: &mut Vec<Face>, face: Face) -> FaceId {
    faces.push(face);
    FaceId(u16::try_from(faces.len() - 1).expect("far fewer than 65536 faces are loaded"))
}

/// Measures a cell of the monospace `face` at `ppem`: as wide as its
/// characters, as tall as its ascent, descent and line gap.
fn measure(face: &Face, ppem: f32) -> CellMetrics {
    let font = face.font();
    let metrics = font.metrics(&[]).scale(ppem);
    let advance = face.glyph('M').map_or(ppem / 2.0, |glyph| {
        font.glyph_metrics(&[]).scale(ppem).advance_width(glyph)
    });
    let height = (metrics.ascent + metrics.descent + metrics.leading).ceil();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    CellMetrics {
        width: advance.round().max(1.0) as u32,
        height: height.max(1.0) as u32,
        // Any line gap is split above and below the text.
        baseline: (metrics.leading / 2.0 + metrics.ascent).round(),
        underline_offset: (-metrics.underline_offset).max(1.0).round(),
        strikeout_offset: metrics.strikeout_offset.round(),
        stroke: metrics.stroke_size.max(1.0).round(),
    }
}
