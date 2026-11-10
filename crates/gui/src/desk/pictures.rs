//! The desk's side of the image layer (CLACKA-239): the images its panes
//! asked for as they were painted, fetched from the daemon a part at a
//! time and handed to the cache to decode (see [`crate::images`]); and
//! its picture panes, each attached to its picture while the tab showing
//! it is shown.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    Event, ImageAnswer, ImageFrom, ImageId, ImageNews, PaneId, Picture, PictureId, PictureState,
    Request, Response, Size, wire,
};

use super::{Asked, Desk};
use crate::images::{Gone, Images};
use crate::input::KeyPress;
use crate::layout::Tile;
use crate::picture::{Ask, PicturePane, STATUS_ROWS};

/// How long a copy being made, or an answer through another daemon that
/// doesn't say when it's ready, is waited for before it's asked again.
const ASK_AGAIN: Duration = Duration::from_secs(1);

/// How many times a copy being made is asked for before it's given up on:
/// each time after twice as long, up to [`ASK_LONGEST`].
const ASK_TRIES: u32 = 8;

/// The longest a copy being made is waited for between asks.
const ASK_LONGEST: Duration = Duration::from_secs(30);

/// How soon the window is drawn again while images are being decoded, to
/// show each once it is.
const DECODE_WAKE: Duration = Duration::from_millis(16);

/// What's asked of an image: which, from whom, and the side of the copy,
/// if it's one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Fetch {
    pub(super) id: ImageId,
    pub(super) copy: Option<u32>,
}

/// An image being fetched.
#[derive(Debug)]
struct Fetching {
    from: ImageFrom,
    /// The bytes come so far.
    bytes: Vec<u8>,
    /// When to ask again, while a copy is made: `None` while a part is
    /// asked for.
    waiting: Option<Instant>,
    /// How many times it's been said to be being made.
    tries: u32,
}

/// The images being fetched, and the picture panes.
#[derive(Debug, Default)]
pub(super) struct Pictures {
    pub(super) images: Images,
    fetching: HashMap<Fetch, Fetching>,
    pub(super) panes: HashMap<PaneId, PicturePane>,
    /// The pictures attached.
    attached: HashSet<PictureId>,
    /// Whether the decoding thread has anything to do.
    decoding: bool,
    /// When to draw again, to show what's being decoded once it is.
    pub(super) wake: Option<Instant>,
}

impl Desk {
    /// The picture pane `pane` is, if it's one.
    pub(crate) fn picture(&self, pane: PaneId) -> Option<&PicturePane> {
        self.pictures.panes.get(&pane)
    }

    /// The window's image layer, which every pane draws its images from.
    pub(crate) fn image_layer(&self) -> &Images {
        &self.pictures.images
    }

    /// Draws images from `images`, shared with the process's other
    /// windows.
    pub(crate) fn share_images(&mut self, images: &Images) {
        self.pictures.images = images.clone();
        for (&terminal, chat) in &mut self.chats {
            chat.view_mut().share_images(images.clone(), terminal);
        }
    }

    /// Makes a picture pane for every pane of the tab shown that shows a
    /// picture, and attaches to each; forgets those gone.
    pub(super) fn attach_shown_pictures(&mut self) {
        let shown: Vec<(PaneId, PictureId)> = (self.shown_tab())
            .map(|tab| {
                (tab.layout.panes().iter())
                    .filter_map(|pane| Some((pane.id, pane.content.picture()?)))
                    .collect()
            })
            .unwrap_or_default();
        let pictures = &mut self.pictures;
        pictures
            .panes
            .retain(|pane, picture| shown.contains(&(*pane, picture.picture())));
        for &(pane, picture) in &shown {
            pictures
                .panes
                .entry(pane)
                .or_insert_with(|| PicturePane::new(picture));
        }
        if !self.connected || !self.daemon_has(wire::IMAGES) {
            return;
        }
        // Those no longer shown, which the daemon may let go of meanwhile.
        let gone: Vec<PictureId> = (self.pictures.attached.iter())
            .filter(|attached| !shown.iter().any(|(_, picture)| picture == *attached))
            .copied()
            .collect();
        for picture in gone {
            self.pictures.attached.remove(&picture);
            self.request(Request::DetachPicture { picture }, Asked::Quiet);
        }
        for (_, picture) in shown {
            if self.pictures.attached.insert(picture) {
                self.request(Request::AttachPicture { picture }, Asked::Picture(picture));
            }
        }
    }

    /// Sizes the picture pane `pane` to its tile.
    pub(super) fn fit_picture(&mut self, pane: PaneId, tile: Tile) {
        if let Some(picture) = self.pictures.panes.get_mut(&pane)
            && let Ok(size) = Size::new(tile.cols, tile.rows)
        {
            picture.resize(size);
        }
    }

    /// Lays out the words of the picture panes in the tab shown.
    pub(crate) fn settle_pictures(&mut self) {
        for picture in self.pictures.panes.values_mut() {
            picture.settle();
        }
    }

    /// The images of the shown tab's picture panes, each in its tile, `cell`
    /// pixels a cell, under its words.
    pub(crate) fn paint_pictures(
        &mut self,
        tiles: &[(PaneId, Tile)],
        cell: [f32; 2],
    ) -> Vec<crate::render::PaintedImage> {
        let images = self.pictures.images.clone();
        let mut painted = Vec::new();
        for (pane, tile) in tiles {
            let Some(picture) = self.pictures.panes.get_mut(pane) else {
                continue;
            };
            let words = f32::from(STATUS_ROWS) * cell[1];
            let [width, height] = tile.cells_size(cell);
            let area = [
                tile.origin[0],
                tile.origin[1] + words,
                width,
                (height - words).max(0.0),
            ];
            painted.extend(picture.paint(&images, area));
        }
        painted
    }

    /// Hands `press` to the focused picture pane, then does what it asks.
    /// Returns whether the window must be redrawn.
    pub(super) fn picture_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let Some(picture) = self.pictures.panes.get_mut(&pane) else {
            return false;
        };
        let mut redraw = false;
        for stroke in crate::vim::plain_strokes(press, false) {
            redraw |= picture.stroke(stroke);
        }
        for ask in picture.take_asks() {
            match ask {
                Ask::Close => self.request(Request::ClosePane { pane }, Asked::Command),
            }
        }
        redraw
    }

    /// Moves about in the picture pane `pane` by `lines` of the wheel.
    pub(super) fn picture_wheel(&mut self, pane: PaneId, lines: i64) -> bool {
        let Some(picture) = self.pictures.panes.get_mut(&pane) else {
            return false;
        };
        #[allow(clippy::cast_precision_loss)]
        let by = lines as f32 * 0.05;
        picture.move_by([0.0, -by])
    }

    /// Takes in what the daemon says the picture `picture` shows.
    pub(super) fn picture_shown(&mut self, picture: PictureId, shown: &Picture) -> bool {
        if let PictureState::Shown(info) = &shown.state {
            self.pictures.images.offered(info.id);
        }
        let mut changed = false;
        for pane in self.pictures.panes.values_mut() {
            if pane.picture() == picture {
                pane.set_shown(shown.clone());
                changed = true;
            }
        }
        changed
    }

    /// Forgets what was attached and asked, as a new connection does.
    pub(super) fn pictures_reconnected(&mut self) {
        self.pictures.attached.clear();
        self.markdown_images_reconnected();
        self.pictures.fetching.clear();
    }

    /// Takes in what came of image work asked for. Returns whether the
    /// window must be redrawn.
    pub(crate) fn image_event(&mut self, event: Event) -> bool {
        match event {
            Event::Picture { picture, shown } => self.picture_shown(picture, &shown),
            Event::Image(ImageNews::Scaled { image, .. }) => {
                // Ask for the copies of it waited for at once.
                let now = Instant::now();
                for (fetch, fetching) in &mut self.pictures.fetching {
                    if fetch.id == image && fetching.waiting.is_some() {
                        fetching.waiting = Some(now);
                    }
                }
                self.fetch_images();
                false
            }
            Event::Image(ImageNews::Found {
                buffer,
                path,
                found,
            }) => {
                self.markdown_image_found(buffer, &path, found);
                true
            }
            _ => false,
        }
    }

    /// Fetches what the window's panes wanted as they were painted, and
    /// has what's come decoded, drawing again soon while anything is.
    /// Returns whether anything was asked of the daemon.
    pub(crate) fn settle_images(&mut self) -> bool {
        let asked = self.asks_sent;
        let images = self.pictures.images.clone();
        // What one daemon can't show, or sent as a copy, is its alone.
        images.on_machine(self.workspaces.machine.as_ref().map(|machine| machine.id));
        let (came, decoding) = images.take_decoded();
        self.pictures.decoding = decoding;
        for ((id, side), from) in images.take_wanted() {
            if images.decode(id, side) {
                self.pictures.decoding = true;
                continue;
            }
            // A window at the far end of a link takes a copy the size it's
            // drawn; one here, the image as it was sent.
            let copy = self.remote.then_some(side);
            let fetch = Fetch { id, copy };
            if self.pictures.fetching.contains_key(&fetch) || !self.connected {
                continue;
            }
            if !self.daemon_has(wire::IMAGES) {
                images.gone(id, Gone::Failed("the daemon is too old".into()));
                continue;
            }
            self.pictures.fetching.insert(
                fetch,
                Fetching {
                    from,
                    bytes: Vec::new(),
                    waiting: None,
                    tries: 0,
                },
            );
            self.ask_for_part(fetch);
        }
        self.fetch_images();
        // Drawn again soon while decoding, and when a copy waited for is to
        // be asked for again, which nothing else may wake the window for.
        let busy = came || self.pictures.decoding;
        let ask_again = (self.pictures.fetching.values())
            .filter_map(|fetching| fetching.waiting)
            .min();
        self.pictures.wake = (busy.then(|| Instant::now() + DECODE_WAKE))
            .into_iter()
            .chain(ask_again)
            .min();
        self.asks_sent != asked
    }

    /// Asks again for the copies waited for long enough.
    fn fetch_images(&mut self) {
        let now = Instant::now();
        let due: Vec<Fetch> = (self.pictures.fetching.iter())
            .filter(|(_, fetching)| fetching.waiting.is_some_and(|due| due <= now))
            .map(|(&fetch, _)| fetch)
            .collect();
        for fetch in due {
            self.ask_for_part(fetch);
        }
    }

    /// Asks for the next part of `fetch`.
    fn ask_for_part(&mut self, fetch: Fetch) {
        let Some(fetching) = self.pictures.fetching.get_mut(&fetch) else {
            return;
        };
        fetching.waiting = None;
        let request = Request::Image {
            from: fetching.from,
            image: fetch.id,
            fit: fetch.copy.map(|side| [side, side]),
            at: fetching.bytes.len() as u64,
        };
        self.request(request, Asked::Image(fetch));
    }

    /// Takes in the daemon's answer to asking for part of `fetch`. Returns
    /// whether the window must be redrawn.
    pub(super) fn image_answered(&mut self, fetch: Fetch, response: Response) -> bool {
        let images = self.pictures.images.clone();
        let Some(fetching) = self.pictures.fetching.get_mut(&fetch) else {
            return false;
        };
        match response {
            Response::Image(ImageAnswer::Part {
                info,
                total,
                at,
                bytes,
            }) if at == fetching.bytes.len() as u64
                && total == info.bytes
                && fetching.bytes.len() as u64 + bytes.len() as u64 <= total =>
            {
                let empty = bytes.is_empty();
                fetching.bytes.extend_from_slice(&bytes);
                if fetching.bytes.len() as u64 == total {
                    let fetched = self.pictures.fetching.remove(&fetch).expect("found above");
                    // An image is what its ID says, which its bytes are
                    // checked against, as other windows draw them too.
                    if fetch.copy.is_none()
                        && clackatoo_daemon::image_id(&fetched.bytes) != fetch.id
                    {
                        images.gone(fetch.id, Gone::Failed("it came changed".into()));
                    } else {
                        images.keep(fetch.id, fetch.copy, info, fetched.bytes);
                    }
                    return true;
                }
                if empty {
                    self.pictures.fetching.remove(&fetch);
                    images.gone(fetch.id, Gone::Failed("it came short".into()));
                    return true;
                }
                self.ask_for_part(fetch);
                false
            }
            // Asked again once it's made, which a daemon passing the
            // answer on from another doesn't say: or after a while.
            Response::Image(ImageAnswer::Scaling) => {
                fetching.tries += 1;
                if fetching.tries > ASK_TRIES {
                    self.pictures.fetching.remove(&fetch);
                    images.gone(fetch.id, Gone::Failed("its copy never came".into()));
                    return true;
                }
                let wait = (ASK_AGAIN * (1 << (fetching.tries - 1).min(16))).min(ASK_LONGEST);
                fetching.waiting = Some(Instant::now() + wait);
                false
            }
            Response::Image(ImageAnswer::Dropped { limit }) => {
                self.pictures.fetching.remove(&fetch);
                images.gone(fetch.id, Gone::Dropped(limit));
                true
            }
            Response::Error(err) => {
                self.pictures.fetching.remove(&fetch);
                images.gone(fetch.id, Gone::Failed(err.to_string()));
                true
            }
            _ => {
                self.pictures.fetching.remove(&fetch);
                images.gone(fetch.id, Gone::Failed("the daemon no longer has it".into()));
                true
            }
        }
    }

    /// Takes in the daemon's answer to attaching to `picture`.
    pub(super) fn picture_answered(&mut self, picture: PictureId, response: Response) -> bool {
        match response {
            Response::Picture(shown) => self.picture_shown(picture, &shown),
            other => {
                log::warn!("the daemon answered a picture with {other:?}");
                self.pictures.attached.remove(&picture);
                false
            }
        }
    }
}
