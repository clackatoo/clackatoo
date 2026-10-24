//! One modal for every image registered by the shared thumbnail renderer.
use super::{
    Canvas, ChatView, Fill, Fonts, Hit, Measures, Paint, Place, TextPaint, Tone, draw_laid,
};

impl ChatView {
    pub(super) fn draw_image_viewer(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
    ) {
        let Some(viewer) = &self.image_viewer else {
            return;
        };
        let image = viewer.preview.clone();
        let original = viewer.picture.clone();
        let status = viewer.status.clone();
        let dimensions = original
            .as_ref()
            .map_or([image.width, image.height], |image| {
                [image.width, image.height]
            });
        self.hits.clear();
        self.shown.clear();
        // Composer remove buttons and other overlay text belong behind the modal.
        paint.over_glyphs.clear();
        paint.ime = None;
        let [left, top] = place.origin;
        let [width, height] = place.size;
        let pad = m.em(1.0).min(width / 8.0).min(height / 8.0);
        let area = [
            left + 2.0 * pad,
            top + 4.0 * pad,
            (width - 4.0 * pad).max(1.0),
            (height - 8.0 * pad).max(1.0),
        ];
        let Some(mut picture) = crate::browser::fit(dimensions, area) else {
            return;
        };
        picture[1] = top + (height - picture[3]) / 2.0;
        let panel_width = (picture[2] + 2.0 * pad).max(m.em(20.0).min(width - 2.0 * pad));
        let bounds = [
            left + (width - panel_width) / 2.0,
            picture[1] - 3.0 * pad,
            panel_width,
            picture[3] + 6.0 * pad,
        ];
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [left, top, width, height],
            em: m.em,
            over: true,
        };
        canvas.fill(Fill::solid(canvas.clip, Paint::ImageBackdrop));
        canvas.fill(
            Fill::rounded(bounds, Paint::Palette, m.em(0.8)).bordered(1.0, Paint::RaisedEdge),
        );
        if let Some(original) = original {
            canvas.paint.images.push(crate::render::PaintedImage {
                over: true,
                image: original,
                rect: picture,
                clip: canvas.clip,
            });
        } else {
            self.previews.draw(&mut canvas, &image, picture);
        }
        let close = [
            bounds[0] + bounds[2] - 3.0 * pad,
            bounds[1] + pad * 0.5,
            2.0 * pad,
            2.0 * pad,
        ];
        let label = self.box_label(canvas.fonts, m, "×", Tone::Plain, close[2]);
        draw_laid(
            &mut canvas,
            m,
            &label,
            [
                close[0] + (close[2] - label.width) / 2.0,
                close[1] + (close[3] - label.height) / 2.0,
            ],
            close[2],
        );
        self.keep_label(label);
        self.hits.push((Hit::CloseImage, place.in_pane(close)));
        let hint = self.box_label(
            canvas.fonts,
            m,
            &format!("{status} · Esc / Ctrl+; closes"),
            Tone::Muted,
            bounds[2] - 2.0 * pad,
        );
        draw_laid(
            &mut canvas,
            m,
            &hint,
            [bounds[0] + pad, picture[1] + picture[3] + pad],
            bounds[2] - 2.0 * pad,
        );
        self.keep_label(hint);
    }
}
