//! Terminals' pictures in the window (CLACKA-239): what the rows each
//! terminal draws show of pictures, placed over or under their text as
//! their programs asked, through the window's one image layer, as chats',
//! Markdown's and picture panes' images are. A picture the daemon let go
//! says so in its place. Text selected or just copied over a picture is lit
//! over it too, so a selection never hides under one.

use std::sync::Arc;

use clackatoo_protocol::{ImageFrom, ImageId, TerminalId, dropped_words};

use crate::desk::Desk;
use crate::images::Shown as Look;
use crate::render::{
    Fill, HighlightKind, Image, Ink, Layer, Paint, PlacedImage, Scene, Text, TextFont, TextureKey,
};

/// A part of a picture, laid out, kept alive until the GPU has it.
pub(super) struct TerminalPicture {
    key: TextureKey,
    image: Arc<Image>,
    layer: Layer,
    rect: [f32; 4],
    crop: [f32; 4],
    clip: [f32; 4],
}

impl TerminalPicture {
    pub(super) fn placed(&self) -> PlacedImage<'_> {
        PlacedImage {
            key: self.key,
            layer: self.layer,
            rect: self.rect,
            crop: Some(self.crop),
            image: &self.image,
            clip: Some(self.clip),
        }
    }
}

/// What terminals' pictures add to the window: the parts drawn, the words
/// in place of those let go, and the light over those selected.
#[derive(Default)]
pub(super) struct Shown {
    pub(super) pictures: Vec<TerminalPicture>,
    pub(super) words: Vec<([f32; 2], String)>,
    pub(super) lit: Vec<Fill>,
}

/// What of pictures each of `terminals`' scenes shows, in cells `cell`
/// pixels big.
pub(super) fn terminal_pictures(
    desk: &Desk,
    terminals: &[(TerminalId, &Scene<'_>)],
    cell: [f32; 2],
) -> Shown {
    let images = desk.image_layer();
    let mut shown = Shown::default();
    for &(terminal, scene) in terminals {
        let Some(pane) = desk.pane(terminal) else {
            continue;
        };
        let from = ImageFrom::Terminal(terminal);
        for (line, slice) in pane.picture_slices(scene.rows.len(), cell) {
            let (top, row_clip) = scene.row_position(line, cell[1]);
            let rect = [
                scene.origin[0] + slice.rect[0] * cell[0],
                top + slice.rect[1] * cell[1],
                slice.rect[2] * cell[0],
                slice.rect[3] * cell[1],
            ];
            // Within the row as it scrolls, and the pane.
            let [x0, y0, x1, y1] = row_clip;
            let clip = crate::markdown::layout::intersection(
                [x0, y0, x1 - x0, y1 - y0],
                [
                    scene.origin[0],
                    scene.origin[1],
                    scene.size[0],
                    scene.size[1],
                ],
            );
            // The whole image's size drawn, for the side it's fetched at.
            #[allow(clippy::cast_precision_loss, reason = "sizes far below 2^24")]
            let whole = (rect[2] / slice.crop[2].max(1.0) * slice.size[0] as f32)
                .max(rect[3] / slice.crop[3].max(1.0) * slice.size[1] as f32);
            match images.show(from, ImageId(slice.image), whole) {
                Look::Ready(image, key) => {
                    // The part in the pixels of the copy decoded.
                    #[allow(clippy::cast_precision_loss, reason = "sizes far below 2^24")]
                    let scale = [
                        image.width as f32 / slice.size[0].max(1) as f32,
                        image.height as f32 / slice.size[1].max(1) as f32,
                    ];
                    let crop = [
                        slice.crop[0] * scale[0],
                        slice.crop[1] * scale[1],
                        slice.crop[2] * scale[0],
                        slice.crop[3] * scale[1],
                    ];
                    let layer = if slice.z < 0 {
                        Layer::UnderText
                    } else {
                        Layer::OverText
                    };
                    shown.lit.extend(lit(scene, line, rect, clip, cell));
                    shown.pictures.push(TerminalPicture {
                        key,
                        image,
                        layer,
                        rect,
                        crop,
                        clip,
                    });
                }
                // Once, at the top of the picture.
                Look::Dropped(limit)
                    if slice.crop[0] == 0.0 && slice.crop[1] == 0.0 && rect[1] >= clip[1] =>
                {
                    shown.words.push(([rect[0], rect[1]], dropped_words(limit)));
                }
                Look::Dropped(_) | Look::Loading | Look::Failed(_) => {}
            }
        }
    }
    shown
}

/// The light over the part of a picture drawn at `rect`, cut off outside
/// `clip`, on line `line` of `scene`, that's selected or just copied.
fn lit(
    scene: &Scene<'_>,
    line: usize,
    rect: [f32; 4],
    clip: [f32; 4],
    cell: [f32; 2],
) -> impl Iterator<Item = Fill> {
    let lit: Vec<Fill> = (scene.highlights.iter())
        .filter(|lit| lit.line == line)
        .filter(|lit| matches!(lit.kind, HighlightKind::Selection | HighlightKind::Copied))
        .filter_map(|lit| {
            #[allow(clippy::cast_precision_loss, reason = "columns are few")]
            let cols = [lit.cols.start as f32, lit.cols.end as f32];
            let left = (scene.origin[0] + cols[0] * cell[0]).max(rect[0]);
            let right = (scene.origin[0] + cols[1] * cell[0]).min(rect[0] + rect[2]);
            (right > left).then(|| Fill {
                clip: Some(clip),
                ..Fill::solid([left, rect[1], right - left, rect[3]], Paint::Selected)
            })
        })
        .collect();
    lit.into_iter()
}

/// `words` as the window's own quiet text.
pub(super) fn words_text(words: &[([f32; 2], String)]) -> impl Iterator<Item = Text<'_>> + '_ {
    words.iter().map(|(origin, content)| Text {
        origin: *origin,
        content,
        ink: Ink::Muted,
        font: TextFont::Mono,
    })
}
