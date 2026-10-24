//! Small chat previews use the existing image pass. Only pictures painted
//! recently are cached; scrolling doesn't keep every GPU texture alive.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::markdown::layout::{Canvas, intersection};
use crate::render::{Image, PaintedImage};
use clackatoo_protocol::ChatPreview;

struct Cached {
    // Keep the source alive: its allocation identifies this cached image.
    source: Arc<[u8]>,
    image: Arc<Image>,
    used: bool,
}

#[derive(Default)]
pub(super) struct Cache {
    entries: RefCell<HashMap<(usize, u32, u32), Cached>>,
    hits: RefCell<Vec<(ChatPreview, [f32; 4])>>,
    origin: [f32; 2],
}

impl Cache {
    pub(super) fn begin(&mut self, origin: [f32; 2]) {
        self.origin = origin;
        self.hits.get_mut().clear();
        self.entries.get_mut().retain(|_, entry| entry.used);
        for entry in self.entries.get_mut().values_mut() {
            entry.used = false;
        }
    }

    /// Every thumbnail source registers through draw, in actual clipped pixels.
    pub(super) fn at(&self, at: [f32; 2]) -> Option<ChatPreview> {
        let at = [at[0] + self.origin[0], at[1] + self.origin[1]];
        self.hits
            .borrow()
            .iter()
            .rev()
            .find(|(_, rect)| super::contains(*rect, at))
            .map(|(image, _)| image.clone())
    }

    pub(super) fn draw(&self, canvas: &mut Canvas<'_>, preview: &ChatPreview, area: [f32; 4]) {
        if !preview.valid() {
            return;
        }
        let Some(rect) = crate::browser::fit([preview.width, preview.height], area) else {
            return;
        };
        let clipped = intersection(rect, canvas.clip);
        if clipped[2] <= 0.0 || clipped[3] <= 0.0 {
            return;
        }
        if !canvas.over {
            self.hits.borrow_mut().push((preview.clone(), clipped));
        }
        let key = (
            preview.rgba.as_ptr() as usize,
            preview.width,
            preview.height,
        );
        let mut cache = self.entries.borrow_mut();
        let entry = cache.entry(key).or_insert_with(|| {
            // Browser slots are pane IDs (u32); chat images use the upper
            // half, unique across all chat panes in this window.
            static NEXT: AtomicU64 = AtomicU64::new(1_u64 << 63);
            Cached {
                source: preview.rgba.clone(),
                image: Arc::new(Image {
                    id: NEXT.fetch_add(1, Ordering::Relaxed),
                    width: preview.width,
                    height: preview.height,
                    rgba: preview.rgba.to_vec(),
                }),
                used: true,
            }
        });
        debug_assert!(Arc::ptr_eq(&entry.source, &preview.rgba));
        entry.used = true;
        canvas.paint.images.push(PaintedImage {
            over: canvas.over,
            image: entry.image.clone(),
            rect,
            clip: canvas.clip,
        });
    }
}

pub(super) fn side(width: f32, count: usize, max: f32, gap: f32) -> f32 {
    let count = f32::from(u8::try_from(count.max(1)).unwrap_or(4));
    ((width - gap * (count - 1.0)) / count).clamp(1.0, max)
}

/// The row is only as tall as its tallest fitted picture, not a square
/// for every landscape screenshot. A missing thumbnail keeps its tile.
pub(super) fn height<'a>(
    side: f32,
    previews: impl Iterator<Item = Option<&'a ChatPreview>>,
) -> f32 {
    previews
        .map(|preview| {
            preview
                .filter(|preview| preview.valid())
                .map_or(side, |preview| {
                    #[allow(clippy::cast_precision_loss)]
                    let ratio = preview.height as f32 / preview.width as f32;
                    (side * ratio).min(side).max(1.0)
                })
        })
        .fold(0.0, f32::max)
}
