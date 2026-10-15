//! Finding fonts, measuring cells, and turning characters into glyphs.
//!
//! The configured family supplies the regular, bold, italic and bold italic
//! faces. A character none of them has is looked up in fallback fonts
//! (CJK, emoji, other scripts), found through fontconfig and cached.
//!
//! A cell holding several codepoints, as a joined emoji or a letter with
//! accents, is shaped: the font's own rules pick and place its glyphs.

use std::collections::HashMap;

use anyhow::Context;
use fontique::{
    Attributes, Blob, Collection, CollectionOptions, FallbackKey, FontStyle, FontWeight,
    GenericFamily, QueryFamily, QueryStatus, Script, SourceCache,
};
use swash::shape::ShapeContext;
use swash::text::Codepoint;
use swash::{CacheKey, FontRef, GlyphId};

/// Joins emoji into one, as 👩 and 💻 into 👩‍💻.
const ZWJ: char = '\u{200D}';
/// Asks for the emoji form of a character that also has a text form, as
/// ❤️ for ❤.
const EMOJI_PRESENTATION: char = '\u{FE0F}';

/// One of the 26 letters that make flags in pairs, as 🇬 and 🇧 make 🇬🇧.
pub(crate) fn is_regional_indicator(ch: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch)
}

/// One of the five skin tones that colour the emoji before them, as 🏽 does
/// 👍 in 👍🏽.
pub(crate) fn is_skin_tone(ch: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&ch)
}

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

/// The interface font, for the window's own text, such as its bars:
/// proportional, so each glyph is placed by its own width.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UiFont {
    regular: FaceId,
    bold: FaceId,
    /// The size, in points.
    points: f32,
    /// Whether these are the terminal's faces, standing in for want of an
    /// interface font.
    stand_in: bool,
    /// The size glyphs are drawn at, in pixels per em.
    pub(crate) ppem: f32,
    /// From the top of a line down to the baseline.
    pub(crate) baseline: f32,
    /// A line's height.
    pub(crate) height: f32,
}

/// One glyph of interface text, and how far it moves the pen.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UiGlyph {
    pub(crate) face: FaceId,
    pub(crate) glyph: GlyphId,
    pub(crate) ppem: f32,
    pub(crate) advance: f32,
}

/// One of the glyphs a cell's codepoints are shaped into, and where it
/// goes from the cell's pen, in pixels, down being positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShapedGlyph {
    pub(crate) face: FaceId,
    pub(crate) glyph: GlyphId,
    pub(crate) offset: [f32; 2],
}

/// The fonts a window draws with, at one pixel size.
pub(crate) struct Fonts {
    /// Every font on the system, found once, for us and for laying out
    /// text with parley.
    pub(crate) font_cx: parley::FontContext,
    /// The faces parley laid text out in, as we know them.
    laid_out: HashMap<(u64, u32, bool, bool), FaceId>,
    /// The terminal's family, as configured, or `None` for the system's
    /// monospace font.
    pub(crate) mono_family: Option<String>,
    /// The interface's family, or `None` for the system's sans-serif font.
    pub(crate) ui_family: Option<String>,
    faces: Vec<Face>,
    /// The four primary faces, by variant, as indices into `faces`.
    primary: [FaceId; 4],
    glyphs: HashMap<(char, Variant), Option<(FaceId, GlyphId)>>,
    /// The emoji face that has each character, for emoji sequences.
    emoji: HashMap<char, Option<FaceId>>,
    shaper: ShapeContext,
    /// The configured size, in points.
    points: f32,
    /// Physical pixels per logical pixel, on the window's display.
    scale: f64,
    /// The size glyphs are drawn at, in pixels per em.
    pub(crate) ppem: f32,
    pub(crate) cell: CellMetrics,
    pub(crate) ui: UiFont,
    /// Counts changes of the terminal's family, so text laid out in the
    /// old one is laid out again. A change of size shows in `ppem`.
    pub(crate) generation: u32,
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
        let mut font_cx = parley::FontContext {
            collection: Collection::new(CollectionOptions::default()),
            source_cache: SourceCache::default(),
        };
        let mut faces = Vec::new();
        let primary = primaries(&mut font_cx, &mut faces, options.family.as_deref())?;

        // Until an interface font is loaded, the terminal's stands in.
        let ui = UiFont {
            regular: primary[0],
            bold: primary[1],
            points: options.size,
            ppem: 0.0,
            baseline: 0.0,
            height: 1.0,
            stand_in: true,
        };
        let mut fonts = Self {
            font_cx,
            laid_out: HashMap::new(),
            mono_family: options.family.clone(),
            ui_family: None,
            faces,
            primary,
            ui,
            glyphs: HashMap::new(),
            emoji: HashMap::new(),
            shaper: ShapeContext::new(),
            points: options.size,
            scale,
            ppem: 0.0,
            cell: CellMetrics {
                width: 1,
                height: 1,
                baseline: 0.0,
                underline_offset: 0.0,
                strikeout_offset: 0.0,
                stroke: 1.0,
            },
            generation: 0,
        };
        fonts.set_scale(scale);
        Ok(fonts)
    }

    /// Draws terminals in `family`, or the system's monospace font, at
    /// `points`, as the `font` and `fontsize` options say, measuring the
    /// cell again. Glyphs drawn in the old font must be thrown away. A
    /// family that isn't installed is refused, and nothing changes.
    pub(crate) fn set_terminal(&mut self, family: Option<&str>, points: f32) -> anyhow::Result<()> {
        if family != self.mono_family.as_deref() {
            let primary = primaries(&mut self.font_cx, &mut self.faces, family)?;
            // Until an interface font is loaded, the terminal's stands in.
            if self.ui.stand_in {
                self.ui.regular = primary[0];
                self.ui.bold = primary[1];
            }
            self.primary = primary;
            self.mono_family = family.map(str::to_owned);
            self.glyphs.clear();
            self.emoji.clear();
            self.generation = self.generation.wrapping_add(1);
        }
        self.points = points;
        self.set_scale(self.scale);
        Ok(())
    }

    /// Sizes text for a display with `scale` physical pixels per logical
    /// pixel. Glyphs rasterised at the old size must be thrown away.
    pub(crate) fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
        // Points are 1/72 inch, and a logical pixel is 1/96 inch.
        #[allow(clippy::cast_possible_truncation)]
        let ppem = (f64::from(self.points) * 96.0 / 72.0 * scale) as f32;
        self.ppem = ppem;
        self.cell = measure(self.face(self.primary[0]), ppem);
        #[allow(clippy::cast_possible_truncation)]
        let ui_ppem = (f64::from(self.ui.points) * 96.0 / 72.0 * scale) as f32;
        let metrics = self
            .face(self.ui.regular)
            .font()
            .metrics(&[])
            .scale(ui_ppem);
        self.ui.ppem = ui_ppem;
        self.ui.height = (metrics.ascent + metrics.descent + metrics.leading)
            .ceil()
            .max(1.0);
        self.ui.baseline = (metrics.leading / 2.0 + metrics.ascent).round();
    }

    /// The fonts a window opens with: the terminal's from `options`, and
    /// the interface's the `desktop` names, both at a pixel a point until
    /// the window says how many its screen has.
    pub(crate) fn for_window(
        options: &FontOptions,
        desktop: &crate::desktop::Desktop,
    ) -> anyhow::Result<Self> {
        let mut fonts = Self::load(options, 1.0)?;
        let (family, points) = match &desktop.font {
            Some((family, points)) => (Some(family.as_str()), *points),
            None => (None, 11.0),
        };
        fonts.load_ui(family, points, 1.0);
        Ok(fonts)
    }

    /// Lays `text` out on a thread of its own in each of `families`, at
    /// the terminal's size, and lets it go: so the families
    /// parley looks through for it, and the fonts behind them for what
    /// they haven't got, are found before the window needs them.
    ///
    /// Finding a family is fontconfig sorting every font on the system
    /// against it, which takes milliseconds a family; the first chat drawn
    /// would otherwise wait for each family its stack goes through. The
    /// thread's copy of the collection shares the system's fonts with this
    /// one, and what it finds of each family is kept there, for both. It
    /// runs once and ends; nothing waits for it. Returns it, if it started.
    pub(crate) fn warm(
        &self,
        families: Vec<parley::FontFamily<'static>>,
        text: String,
    ) -> Option<std::thread::JoinHandle<()>> {
        let collection = self.font_cx.collection.clone();
        let size = self.ppem;
        let spawned = std::thread::Builder::new()
            .name("fonts".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let mut font_cx = parley::FontContext {
                    collection,
                    source_cache: SourceCache::default(),
                };
                let mut layout_cx = parley::LayoutContext::<()>::new();
                for family in families {
                    let mut builder = layout_cx.ranged_builder(&mut font_cx, &text, 1.0, true);
                    builder.push_default(parley::StyleProperty::FontFamily(family));
                    builder.push_default(parley::StyleProperty::FontSize(size));
                    builder.build(&text);
                }
                log::debug!("fonts found ahead in {:.1?}", started.elapsed());
            });
        spawned
            .inspect_err(|err| log::warn!("couldn't find fonts ahead: {err}"))
            .ok()
    }

    /// Loads the interface font at `points`: `family`, or if it isn't
    /// installed, as when the desktop names one it doesn't have, the first
    /// of the [`ui_families`] that is. Without any, the terminal's font
    /// stays in its place.
    pub(crate) fn load_ui(&mut self, family: Option<&str>, points: f32, scale: f64) {
        let mut found = None;
        for family in ui_families(family) {
            let mut load = |variant| {
                query(
                    &mut self.font_cx.collection,
                    &mut self.font_cx.source_cache,
                    family,
                    variant,
                )
            };
            if let Some(regular) = load(Variant::Regular) {
                found = Some((family, regular, load(Variant::Bold)));
                break;
            }
            log::info!("no interface font {family:?}; trying the next");
        }
        let Some((family, regular, bold)) = found else {
            log::warn!("no interface font found, not even a sans-serif one");
            return;
        };
        let regular = push_face(&mut self.faces, regular);
        self.ui.regular = regular;
        self.ui.bold = bold.map_or(regular, |bold| push_face(&mut self.faces, bold));
        self.ui.stand_in = false;
        self.ui_family = family_name(family);
        self.ui.points = points;
        self.set_scale(scale);
    }

    /// The glyph for `ch` in interface text: from the interface font, or
    /// else the terminal's fonts, a cell wide.
    pub(crate) fn ui_glyph(&mut self, ch: char, bold: bool) -> Option<UiGlyph> {
        let face_id = if bold { self.ui.bold } else { self.ui.regular };
        let face = self.face(face_id);
        if let Some(glyph) = face.glyph(ch) {
            let advance = face
                .font()
                .glyph_metrics(&[])
                .scale(self.ui.ppem)
                .advance_width(glyph);
            return Some(UiGlyph {
                face: face_id,
                glyph,
                ppem: self.ui.ppem,
                advance,
            });
        }
        let variant = if bold {
            Variant::Bold
        } else {
            Variant::Regular
        };
        let (face, glyph) = self.glyph(ch, variant)?;
        #[allow(clippy::cast_precision_loss)]
        let advance = self.cell.width as f32;
        Some(UiGlyph {
            face,
            glyph,
            ppem: self.ppem,
            advance,
        })
    }

    /// How wide `text` is in the interface font.
    pub(crate) fn ui_width(&mut self, text: &str, bold: bool) -> f32 {
        text.chars()
            .filter_map(|ch| self.ui_glyph(ch, bold))
            .map(|glyph| glyph.advance.round())
            .sum()
    }

    /// The face parley laid text out in: `font`, drawn thicker or
    /// slanted as `synthesis` says.
    pub(crate) fn face_for(
        &mut self,
        font: &parley::FontData,
        synthesis: fontique::Synthesis,
    ) -> Option<FaceId> {
        let key = (
            font.data.id(),
            font.index,
            synthesis.embolden(),
            synthesis.skew().is_some(),
        );
        if let Some(&id) = self.laid_out.get(&key) {
            return Some(id);
        }
        let font_ref = FontRef::from_index(font.data.as_ref(), usize::try_from(font.index).ok()?)?;
        let id = push_face(
            &mut self.faces,
            Face {
                blob: font.data.clone(),
                offset: font_ref.offset,
                key: font_ref.key,
                offset_index: font.index,
                embolden: key.2,
                skew: key.3,
            },
        );
        self.laid_out.insert(key, id);
        Some(id)
    }

    /// How many faces are loaded.
    #[cfg(test)]
    pub(crate) fn faces(&self) -> usize {
        self.faces.len()
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
        self.search(
            ch,
            variant,
            &[
                QueryFamily::Generic(GenericFamily::Emoji),
                QueryFamily::Generic(GenericFamily::SansSerif),
            ],
            Some(FallbackKey::new(script, None)),
        )
    }

    /// The first font of `families`, or else of `fallbacks`, that has
    /// `ch`.
    fn search(
        &mut self,
        ch: char,
        variant: Variant,
        families: &[QueryFamily<'_>],
        fallbacks: Option<FallbackKey>,
    ) -> Option<(FaceId, GlyphId)> {
        let mut found = None;
        let mut query = self
            .font_cx
            .collection
            .query(&mut self.font_cx.source_cache);
        query.set_families(families.iter().copied());
        query.set_attributes(variant.attributes());
        if let Some(fallbacks) = fallbacks {
            query.set_fallbacks(fallbacks);
        }
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

    /// The glyphs `text`, one cell's codepoints or a run of cells drawn as
    /// one, is drawn with, shaped in the face its first character is found
    /// in: an emoji face for an emoji sequence. `None` if that face lacks
    /// any of it, as when a mark is only in another font.
    pub(crate) fn shape(&mut self, text: &str, variant: Variant) -> Option<Vec<ShapedGlyph>> {
        let first = text.chars().next()?;
        // A text font may have the emoji a skin tone colours, as ☝, but
        // not the tone or the coloured emoji.
        let emoji = text
            .contains(|ch| {
                matches!(ch, ZWJ | EMOJI_PRESENTATION)
                    || is_regional_indicator(ch)
                    || is_skin_tone(ch)
            })
            .then(|| self.emoji_face(first))
            .flatten();
        let face = match emoji {
            Some(face) => face,
            None => self.glyph(first, variant)?.0,
        };
        let font = self.faces[usize::from(face.0)].font();
        let mut shaper = self
            .shaper
            .builder(font)
            .script(first.script())
            .size(self.ppem)
            .build();
        // The selector has done its job in picking the face, and fonts
        // rarely have a glyph for it.
        for piece in text.split(EMOJI_PRESENTATION) {
            shaper.add_str(piece);
        }
        let mut glyphs = Vec::new();
        let mut pen = 0.0;
        let mut missing = false;
        shaper.shape_with(|cluster| {
            for glyph in cluster.glyphs {
                missing |= glyph.id == 0;
                glyphs.push(ShapedGlyph {
                    face,
                    glyph: glyph.id,
                    offset: [(pen + glyph.x).round(), -glyph.y.round()],
                });
                pen += glyph.advance;
            }
        });
        (!missing).then_some(glyphs)
    }

    /// The emoji face that has `ch`, if any.
    fn emoji_face(&mut self, ch: char) -> Option<FaceId> {
        if let Some(&found) = self.emoji.get(&ch) {
            return found;
        }
        let emoji = [QueryFamily::Generic(GenericFamily::Emoji)];
        let found = self
            .search(ch, Variant::Regular, &emoji, None)
            .map(|(face, _)| face);
        self.emoji.insert(ch, found);
        found
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

/// The regular, bold, italic and bold italic faces of `family`, or the
/// system's monospace font, as indices into `faces`, adding those it
/// hasn't got.
fn primaries(
    font_cx: &mut parley::FontContext,
    faces: &mut Vec<Face>,
    family: Option<&str>,
) -> anyhow::Result<[FaceId; 4]> {
    let family = match family {
        Some(name) => QueryFamily::Named(name),
        None => QueryFamily::Generic(GenericFamily::Monospace),
    };
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
        let face = query(
            &mut font_cx.collection,
            &mut font_cx.source_cache,
            family,
            variant,
        )
        .with_context(|| match family {
            QueryFamily::Named(name) => format!("there's no font called `{name}`"),
            _ => "there's no monospace font".to_owned(),
        })?;
        primary[slot] = face_id(faces, face);
    }
    Ok(primary)
}

/// `face`'s index in `faces`, adding it if it isn't there, so a family
/// chosen again doesn't add its faces again.
fn face_id(faces: &mut Vec<Face>, face: Face) -> FaceId {
    let same = |known: &Face| {
        known.blob.id() == face.blob.id()
            && known.offset_index == face.offset_index
            && known.embolden == face.embolden
            && known.skew == face.skew
    };
    match faces.iter().position(same) {
        Some(index) => FaceId(u16::try_from(index).expect("far fewer than 65536 faces are loaded")),
        None => push_face(faces, face),
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

/// Common sans-serif families, for the interface when neither the family
/// the desktop names nor the system's sans-serif font can be found: GNOME's
/// and KDE's, and those most distributions install.
const SANS_FAMILIES: [&str; 6] = [
    "Noto Sans",
    "Cantarell",
    "Inter",
    "DejaVu Sans",
    "Liberation Sans",
    "Roboto",
];

/// The families to try for the interface, best first: `named`, if given,
/// then the system's sans-serif font, as fontconfig chooses it, so the
/// bars match the chat, which falls back to it too; then common sans-serif
/// families by name, in case the system says of none.
fn ui_families(named: Option<&str>) -> impl Iterator<Item = QueryFamily<'_>> {
    named
        .map(QueryFamily::Named)
        .into_iter()
        .chain([QueryFamily::Generic(GenericFamily::SansSerif)])
        .chain(SANS_FAMILIES.map(QueryFamily::Named))
}

/// The name of `family`, if it's named, rather than generic.
fn family_name(family: QueryFamily<'_>) -> Option<String> {
    match family {
        QueryFamily::Named(name) => Some(name.to_owned()),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interface_tries_the_family_named_then_the_systems_sans_then_common_ones() {
        let tried: Vec<QueryFamily<'_>> = ui_families(Some("Adwaita Sans")).collect();
        assert_eq!(tried[0], QueryFamily::Named("Adwaita Sans"));
        assert_eq!(tried[1], QueryFamily::Generic(GenericFamily::SansSerif));
        assert_eq!(tried[2..], SANS_FAMILIES.map(QueryFamily::Named));
        // With none named, the system's comes first.
        let tried: Vec<QueryFamily<'_>> = ui_families(None).collect();
        assert_eq!(tried[0], QueryFamily::Generic(GenericFamily::SansSerif));
    }

    #[test]
    fn a_family_not_installed_falls_back_to_a_sans_serif_one_not_the_terminals() {
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a monospace font");
        // Only a machine with no sans-serif font at all has nothing but
        // the terminal's to stand in.
        let any_sans = ui_families(None).any(|family| {
            query(
                &mut fonts.font_cx.collection,
                &mut fonts.font_cx.source_cache,
                family,
                Variant::Regular,
            )
            .is_some()
        });
        if !any_sans {
            eprintln!("no sans-serif font installed; skipping");
            return;
        }
        fonts.load_ui(Some("No Such Family Anywhere"), 11.0, 1.0);
        assert!(!fonts.ui.stand_in, "a sans-serif font stands in");
        assert_ne!(fonts.ui.regular, fonts.primary[0]);
        // The system's own, which the chat takes too, or one named.
        let family = fonts.ui_family.as_deref();
        assert!(
            family.is_none_or(|family| SANS_FAMILIES.contains(&family)),
            "{family:?}"
        );
        // Proportional, as the terminal's isn't: an i narrower than an m.
        let advance = |fonts: &mut Fonts, ch| fonts.ui_glyph(ch, false).expect("a glyph").advance;
        assert!(advance(&mut fonts, 'i') < advance(&mut fonts, 'm'));
        assert!(fonts.ui.height > 1.0);
    }
}
