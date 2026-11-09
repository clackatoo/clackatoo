//! A picture pane: an image file, as `:e plan.png` or picking one in an
//! explorer opens it (CLACKA-239). The daemon of the file's machine reads
//! it; the pane shows it through the image layer (see [`crate::images`]),
//! fitted to the pane, or zoomed and moved about in, which is the
//! window's own, as a pane's scroll is.
//!
//! Its first line says the file, the image's size and how it's zoomed;
//! below it, the image.
//!
//! | Key | Does |
//! |---|---|
//! | `+` `-` | Zooms in or out |
//! | `=` | Fits it to the pane |
//! | `0` | Shows it at its own size, a pixel for a pixel |
//! | `h` `j` `k` `l`, the arrows, the wheel | Move about in it, zoomed |
//! | `g` `G` | Its top, or its bottom |
//! | `q` | Closes the pane |

use clackatoo_protocol::{
    Attributes, Color, ImageFrom, ImageInfo, Picture, PictureId, PictureState, Size, Style,
};
use clackatoo_term::{Row, RowBuilder};

use crate::colors;
use crate::images::{Images, Shown};
use crate::render::PaintedImage;
use crate::vim::Stroke;

/// How many lines the pane's words take, above the image.
pub(crate) const STATUS_ROWS: u16 = 1;

/// How much a zoom in or out changes the size.
const ZOOM_STEP: f32 = 1.25;

/// How small and big an image can be zoomed to, as pixels shown per
/// pixel of it.
const ZOOMS: (f32, f32) = (1.0 / 64.0, 64.0);

/// How far a move goes, as a share of the pane.
const MOVE: f32 = 0.125;

/// What the pane asks of the desk, which it can't do itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Close the pane.
    Close,
}

/// How big the image is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Zoom {
    /// As big as fits the pane, in proportion, but never bigger than its
    /// own size.
    Fit,
    /// So many pixels shown for each of its own, centred on `centre`, a
    /// point of it, in its pixels.
    At { scale: f32, centre: [f32; 2] },
}

/// A pane showing an image file.
#[derive(Debug)]
pub(crate) struct PicturePane {
    picture: PictureId,
    /// What the daemon says the pane shows, once it has.
    shown: Option<Picture>,
    zoom: Zoom,
    /// The area the image was last placed in, left, top, width and
    /// height, in pixels: what keys move by.
    area: [f32; 4],
    /// Why its image isn't drawn, if the daemon let it go or it can't be
    /// decoded: said in its words.
    missing: Option<String>,
    width: usize,
    rows: Vec<Row>,
    stale: bool,
    asks: Vec<Ask>,
}

impl PicturePane {
    pub(crate) fn new(picture: PictureId) -> Self {
        Self {
            picture,
            shown: None,
            zoom: Zoom::Fit,
            area: [0.0; 4],
            missing: None,
            width: 0,
            rows: Vec::new(),
            stale: true,
            asks: Vec::new(),
        }
    }

    /// The picture it shows.
    pub(crate) fn picture(&self) -> PictureId {
        self.picture
    }

    /// What it shows is now `shown`, as the daemon says.
    pub(crate) fn set_shown(&mut self, shown: Picture) {
        let image = |shown: Option<&Picture>| match shown.map(|shown| &shown.state) {
            Some(PictureState::Shown(info)) => Some(info.id),
            _ => None,
        };
        // Another image fits the pane again.
        if image(self.shown.as_ref()) != image(Some(&shown)) {
            self.zoom = Zoom::Fit;
        }
        self.shown = Some(shown);
        self.stale = true;
    }

    /// The image it shows, once it's read.
    pub(crate) fn info(&self) -> Option<ImageInfo> {
        match &self.shown.as_ref()?.state {
            PictureState::Shown(info) => Some(*info),
            PictureState::Loading | PictureState::Failed(_) => None,
        }
    }

    /// What the window's title says: the file.
    pub(crate) fn title(&self) -> &str {
        self.shown
            .as_ref()
            .map_or("image", |shown| shown.path.as_str())
    }

    /// What the bar says.
    pub(crate) fn label(&self) -> String {
        format!("{}  ·  + - zoom, = fits, q closes", self.title())
    }

    pub(crate) fn resize(&mut self, size: Size) {
        let width = usize::from(size.cols());
        self.stale |= width != self.width;
        self.width = width;
    }

    /// How many pixels are shown for each of the image's own, in `area`.
    fn scale(&self, info: ImageInfo, area: [f32; 4]) -> f32 {
        match self.zoom {
            Zoom::Fit => fit_scale(info, area),
            Zoom::At { scale, .. } => scale,
        }
    }

    /// Lays out the words, if they changed.
    pub(crate) fn settle(&mut self) {
        if !std::mem::take(&mut self.stale) {
            return;
        }
        let faint = colors::line_number(false);
        let bold = Style {
            attributes: Attributes::BOLD,
            ..Style::DEFAULT
        };
        let mut row = RowBuilder::new(self.width);
        row.push(self.title(), &bold, true);
        match self.shown.as_ref().map(|shown| &shown.state) {
            Some(PictureState::Shown(_)) if let Some(missing) = &self.missing => {
                row.push(&format!("  {missing}"), &faint, true);
            }
            Some(PictureState::Shown(info)) => {
                let format = format!("{:?}", info.format).to_uppercase();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let percent = (self.scale(*info, self.area) * 100.0).round() as u32;
                let zoom = match self.zoom {
                    Zoom::Fit => format!("fitted, {percent}%"),
                    Zoom::At { .. } => format!("{percent}%"),
                };
                let said = format!("  {} × {} {format}  ·  {zoom}", info.width, info.height);
                row.push(&said, &faint, true);
            }
            Some(PictureState::Failed(why)) => {
                let style = Style {
                    fg: Color::Indexed(1),
                    ..Style::DEFAULT
                };
                row.push(&format!("  can't be shown: {why}"), &style, true);
            }
            Some(PictureState::Loading) | None => row.push("  reading…", &faint, true),
        }
        self.rows = vec![row.finish(false)];
    }

    /// The rows to draw: the words. The image is placed under them.
    pub(crate) fn scene(&self) -> Vec<&Row> {
        self.rows.iter().collect()
    }

    /// Where the image goes in `area`, left, top, width and height, in
    /// pixels, as it's zoomed: perhaps bigger than the area, which cuts it
    /// off.
    pub(crate) fn place(&self, info: ImageInfo, area: [f32; 4]) -> Option<[f32; 4]> {
        let [left, top, width, height] = area;
        if width < 1.0 || height < 1.0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let (w, h) = (info.width as f32, info.height as f32);
        let scale = self.scale(info, area);
        let (shown_w, shown_h) = ((w * scale).round().max(1.0), (h * scale).round().max(1.0));
        let centre = match self.zoom {
            Zoom::Fit => [w / 2.0, h / 2.0],
            Zoom::At { centre, .. } => centre,
        };
        Some([
            (left + width / 2.0 - centre[0] * scale).round(),
            (top + height / 2.0 - centre[1] * scale).round(),
            shown_w,
            shown_h,
        ])
    }

    /// The image, painted in `area`, the pane below its words, through
    /// `images`: or, until it's come, nothing, the words saying why.
    pub(crate) fn paint(&mut self, images: &Images, area: [f32; 4]) -> Option<PaintedImage> {
        if self.area.map(f32::to_bits) != area.map(f32::to_bits) {
            self.area = area;
            self.stale = true;
        }
        let info = self.info()?;
        let rect = self.place(info, area)?;
        let from = ImageFrom::Picture(self.picture);
        // Decoded at the size it's drawn, cut off at the pane.
        let drawn = rect[2].max(rect[3]).min(area[2].max(area[3]) * 4.0);
        let (painted, missing) = match images.show(from, info.id, drawn) {
            Shown::Ready(image, key) => {
                let painted = PaintedImage {
                    over: false,
                    key,
                    image,
                    rect,
                    clip: area,
                };
                (Some(painted), None)
            }
            Shown::Loading => (None, None),
            Shown::Dropped(limit) => (None, Some(clackatoo_protocol::dropped_words(limit))),
            Shown::Failed(why) => (None, Some(format!("can't be shown: {why}"))),
        };
        if self.missing != missing {
            self.missing = missing;
            self.stale = true;
        }
        painted
    }

    /// Zooms by `by`, about the centre of what's shown.
    fn zoom_by(&mut self, by: f32) {
        let Some(info) = self.info() else {
            return;
        };
        let (scale, centre) = match self.zoom {
            #[allow(clippy::cast_precision_loss)]
            Zoom::Fit => (
                fit_scale(info, self.area),
                [info.width as f32 / 2.0, info.height as f32 / 2.0],
            ),
            Zoom::At { scale, centre } => (scale, centre),
        };
        let scale = (scale * by).clamp(ZOOMS.0, ZOOMS.1);
        self.zoom = Zoom::At { scale, centre };
        self.stale = true;
    }

    /// Moves what's shown by `by`, shares of the pane, across and down:
    /// only zoomed, as a fitted image shows whole.
    pub(crate) fn move_by(&mut self, by: [f32; 2]) -> bool {
        let Some(info) = self.info() else {
            return false;
        };
        let Zoom::At { scale, centre } = &mut self.zoom else {
            return false;
        };
        #[allow(clippy::cast_precision_loss)]
        let (w, h) = (info.width as f32, info.height as f32);
        let before = centre.map(f32::to_bits);
        centre[0] = (centre[0] + by[0] * self.area[2] / *scale).clamp(0.0, w);
        centre[1] = (centre[1] + by[1] * self.area[3] / *scale).clamp(0.0, h);
        centre.map(f32::to_bits) != before
    }

    /// Moves to the image's top or bottom, zoomed.
    fn go_to_end(&mut self, bottom: bool) -> bool {
        let Some(info) = self.info() else {
            return false;
        };
        let Zoom::At { centre, .. } = &mut self.zoom else {
            return false;
        };
        #[allow(clippy::cast_precision_loss)]
        let h = info.height as f32;
        centre[1] = if bottom { h } else { 0.0 };
        true
    }

    // Takes `stroke`. Returns whether the pane must be drawn again.
    crate::shortcuts::bindings::described_keys! {
        pub(crate) fn stroke / describe_stroke(&mut self, stroke: Stroke) -> bool;
        match stroke => {
            Stroke::Char {
                ch: 'q',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Close pane"),
                {
                    self.asks.push(Ask::Close);
                    true
                };
            Stroke::Char {
                ch: '+',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Zoom in"),
                {
                    self.zoom_by(ZOOM_STEP);
                    true
                };
            Stroke::Char {
                ch: '-',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Zoom out"),
                {
                    self.zoom_by(1.0 / ZOOM_STEP);
                    true
                };
            Stroke::Char {
                ch: '=',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Fit it to the pane"),
                {
                    self.zoom = Zoom::Fit;
                    self.stale = true;
                    true
                };
            Stroke::Char {
                ch: '0',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Show it at its own size"),
                {
                    self.zoom_by(1.0);
                    if let Zoom::At { scale, .. } = &mut self.zoom {
                        *scale = 1.0;
                    }
                    true
                };
            Stroke::Char {
                ch: 'h',
                ctrl: false,
            } | Stroke::Left =>
                crate::shortcuts::bindings::Hint::Action("Move left"),
                self.move_by([-MOVE, 0.0]);
            Stroke::Char {
                ch: 'l',
                ctrl: false,
            } | Stroke::Right =>
                crate::shortcuts::bindings::Hint::Action("Move right"),
                self.move_by([MOVE, 0.0]);
            Stroke::Char {
                ch: 'k',
                ctrl: false,
            } | Stroke::Up =>
                crate::shortcuts::bindings::Hint::Action("Move up"),
                self.move_by([0.0, -MOVE]);
            Stroke::Char {
                ch: 'j',
                ctrl: false,
            } | Stroke::Down =>
                crate::shortcuts::bindings::Hint::Action("Move down"),
                self.move_by([0.0, MOVE]);
            Stroke::Char {
                ch: 'g',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Go to its top"),
                self.go_to_end(false);
            Stroke::Char {
                ch: 'G',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Go to its bottom"),
                self.go_to_end(true);
            _ =>
                crate::shortcuts::bindings::Hint::None,
                false;
        }
        result
    }

    pub(crate) fn shortcut_context(&self) -> crate::shortcuts::Context {
        crate::shortcuts::bindings::list("Image pane", None, |stroke, _| {
            self.describe_stroke(stroke)
        })
    }

    /// What the pane asks of the desk, oldest first.
    pub(crate) fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asks)
    }
}

/// How many pixels are shown for each of the image `info` describes, as
/// big as fits `area`, in proportion, but never bigger than its own size.
fn fit_scale(info: ImageInfo, area: [f32; 4]) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let (w, h) = (info.width.max(1) as f32, info.height.max(1) as f32);
    (area[2] / w).min(area[3] / h).clamp(f32::MIN_POSITIVE, 1.0)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "places are whole pixels")]
mod tests {
    use super::*;
    use clackatoo_protocol::{ImageFormat, ImageId};

    fn shown(width: u32, height: u32) -> PicturePane {
        let mut pane = PicturePane::new(PictureId(1));
        pane.set_shown(Picture {
            path: "~/plan.png".into(),
            state: PictureState::Shown(ImageInfo {
                id: ImageId([1; 16]),
                width,
                height,
                format: ImageFormat::Png,
                bytes: 10,
            }),
        });
        pane.area = [0.0, 20.0, 400.0, 300.0];
        pane
    }

    fn press(pane: &mut PicturePane, ch: char) -> bool {
        pane.stroke(Stroke::Char { ch, ctrl: false })
    }

    #[test]
    fn an_image_is_fitted_centred_and_never_made_bigger_than_it_is() {
        let pane = shown(800, 300);
        let info = pane.info().unwrap();
        assert_eq!(pane.place(info, pane.area), Some([0.0, 95.0, 400.0, 150.0]));
        let small = shown(100, 50);
        let info = small.info().unwrap();
        assert_eq!(
            small.place(info, small.area),
            Some([150.0, 145.0, 100.0, 50.0])
        );
    }

    #[test]
    fn keys_zoom_and_move_about_in_it() {
        let mut pane = shown(800, 600);
        let info = pane.info().unwrap();
        // A fitted image doesn't move.
        assert!(!press(&mut pane, 'l'));
        assert!(press(&mut pane, '0'));
        assert_eq!(
            pane.place(info, pane.area),
            Some([-200.0, -130.0, 800.0, 600.0])
        );
        assert!(press(&mut pane, 'l'));
        // An eighth of the pane, 50 pixels, to the right.
        assert_eq!(
            pane.place(info, pane.area),
            Some([-250.0, -130.0, 800.0, 600.0])
        );
        assert!(press(&mut pane, 'G'));
        assert_eq!(pane.place(info, pane.area).unwrap()[1], 170.0 - 600.0);
        assert!(press(&mut pane, '+'));
        assert_eq!(pane.place(info, pane.area).unwrap()[2], 1000.0);
        assert!(press(&mut pane, '='));
        assert_eq!(pane.place(info, pane.area), Some([0.0, 20.0, 400.0, 300.0]));
        assert!(press(&mut pane, 'q'));
        assert_eq!(pane.take_asks(), vec![Ask::Close]);
    }

    #[test]
    fn the_words_say_the_file_its_size_and_zoom_or_why_not() {
        let mut pane = shown(800, 600);
        pane.resize(Size::new(80, 1).unwrap());
        pane.settle();
        let text = pane.rows[0].text();
        assert!(text.contains("~/plan.png"), "{text}");
        assert!(text.contains("800 × 600 PNG"), "{text}");
        assert!(text.contains("fitted, 50%"), "{text}");
        pane.set_shown(Picture {
            path: "~/notes.txt.png".into(),
            state: PictureState::Failed("it isn't an image".into()),
        });
        pane.settle();
        assert!(
            pane.rows[0]
                .text()
                .contains("can't be shown: it isn't an image")
        );
    }
}
