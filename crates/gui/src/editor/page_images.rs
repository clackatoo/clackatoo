//! The images a Markdown file's rendered view shows (CLACKA-239): each
//! `![description](path)` with a path from the file's folder, read by the
//! daemon of the file's machine as any file a folder from anyone holds is
//! (`Request::ImageAt`), and drawn through the image layer below the
//! block that holds it, as wide as it is, or the column. The description
//! stays where it was, marked as an image's, so what can't be shown, an
//! image from the web, or one the daemon can't read, still says what it
//! was. Nothing is fetched from the web.

use std::collections::HashMap;

use clackatoo_protocol::{ImageFrom, ImageInfo};

use crate::images::{Images, Shown};
use crate::markdown::layout::{Canvas, Laid, Measures, intersection, overlaps};
use crate::render::{Fill, Paint, PaintedImage};

/// The most a path of an image is, in bytes, that's asked for.
const LONGEST_PATH: usize = 1024;

/// How an image the page names is going.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PageImage {
    /// Asked of the daemon.
    Asked,
    /// Read: what it is.
    Found(ImageInfo),
    /// It can't be shown: its description stays, marked.
    Failed,
}

/// The images a page shows, by their paths as its Markdown gives them.
#[derive(Debug, Default)]
pub(super) struct PageImages {
    known: HashMap<String, PageImage>,
    /// Paths to ask the daemon for.
    asks: Vec<String>,
    /// What's known of one changed since the page was laid out.
    pub(super) changed: bool,
}

/// An image placed on a page: from the left of its column and the page's
/// top, in pixels.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Placed {
    pub(super) info: ImageInfo,
    pub(super) rect: [f32; 4],
}

impl PageImages {
    /// Whether `path` names an image the daemon is asked for: one from the
    /// file's folder, not the web's, nor an absolute path.
    pub(super) fn asked_for(path: &str) -> bool {
        !path.is_empty()
            && path.len() <= LONGEST_PATH
            && !path.contains(':')
            && !path.starts_with('/')
            && !path.starts_with('~')
            && !path.starts_with('#')
            && !path.contains('\0')
    }

    /// Makes room for the images on `laid`, a page `column` wide, below the
    /// blocks naming them, moving what's after each down: those known to
    /// be read. Notes those not known yet, to ask for. Returns where each
    /// goes. Images in tables' cells stay their descriptions.
    pub(super) fn make_room(&mut self, laid: &mut Laid, m: Measures, column: f32) -> Vec<Placed> {
        let mut placed = Vec::new();
        let mut shift = 0.0;
        let gap = m.em(0.5);
        let mut table_shift = vec![0.0; laid.tables.len()];
        for index in 0..laid.blocks.len() {
            let table = laid.table_of(index);
            laid.blocks[index].top += shift;
            if let Some(table) = table {
                if laid.tables[table].blocks.start == index {
                    table_shift[table] = shift;
                }
                continue;
            }
            let block = &laid.blocks[index];
            let mut below = block.top + block.layout.height();
            for link in block.links.iter().filter(|link| link.image) {
                let path = link.to.as_str();
                if !Self::asked_for(path) {
                    continue;
                }
                let info = match self.known.get(path) {
                    Some(PageImage::Found(info)) => *info,
                    Some(PageImage::Asked | PageImage::Failed) => continue,
                    None => {
                        self.known.insert(path.to_owned(), PageImage::Asked);
                        self.asks.push(path.to_owned());
                        continue;
                    }
                };
                #[allow(clippy::cast_precision_loss)]
                let (w, h) = (info.width as f32, info.height as f32);
                let width = w.min(column - block.left).max(1.0).round();
                let height = (h * width / w.max(1.0)).round().max(1.0);
                let rect = [block.left, below + gap, width, height];
                below += gap + height;
                shift += gap + height;
                placed.push(Placed { info, rect });
            }
        }
        for (table, moved) in laid.tables.iter_mut().zip(table_shift) {
            table.top += moved;
        }
        laid.height += shift;
        self.changed = false;
        placed
    }

    /// The paths to ask the daemon for, as found laying out.
    pub(super) fn take_asks(&mut self) -> Vec<String> {
        std::mem::take(&mut self.asks)
    }

    /// The daemon read the image at `path`, or couldn't: shown, or its
    /// description stays.
    pub(super) fn found(&mut self, path: &str, found: Option<ImageInfo>) -> bool {
        let Some(known) = self.known.get_mut(path) else {
            return false;
        };
        let now = found.map_or(PageImage::Failed, PageImage::Found);
        if *known == now {
            return false;
        }
        *known = now;
        self.changed = true;
        true
    }

    /// Forgets what was asked and not answered, as a new connection does,
    /// so it's asked again.
    pub(super) fn reconnected(&mut self) {
        self.known.retain(|_, known| *known != PageImage::Asked);
        self.changed = true;
    }
}

/// Draws the images `placed` on a page whose column's top left is at
/// `origin`, `from`'s, through `images`: those not come yet, a box where
/// they'll go.
pub(super) fn draw(
    canvas: &mut Canvas<'_>,
    m: Measures,
    (images, from): (&Images, ImageFrom),
    placed: &[Placed],
    origin: [f32; 2],
) {
    for image in placed {
        let [left, top, width, height] = image.rect;
        let rect = [origin[0] + left, origin[1] + top, width, height];
        if !overlaps(canvas.clip, rect) {
            continue;
        }
        match images.show(from, image.info.id, width.max(height)) {
            Shown::Ready(picture, key) => canvas.paint.images.push(PaintedImage {
                over: canvas.over,
                key,
                image: picture,
                rect,
                clip: canvas.clip,
            }),
            Shown::Loading | Shown::Dropped(_) | Shown::Failed(_) => {
                let shown = intersection(rect, canvas.clip);
                canvas.fill(Fill::rounded(shown, Paint::Code, m.em(0.3)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_from_the_files_folder_are_asked_for() {
        for path in ["a.png", "img/a.png", "../docs/a.png", "./a.png"] {
            assert!(PageImages::asked_for(path), "{path}");
        }
        for path in [
            "https://example.com/a.png",
            "/etc/a.png",
            "~/a.png",
            "file:///a.png",
            "data:image/png;base64,AAAA",
            "#heading",
            "",
        ] {
            assert!(!PageImages::asked_for(path), "{path}");
        }
        assert!(!PageImages::asked_for(&"a".repeat(LONGEST_PATH + 1)));
    }
}
