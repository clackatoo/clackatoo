//! Images the chats show from the daemon's images (CLACKA-239): each is
//! asked for once it's on the screen, at about the size it's shown, in
//! parts, and kept as a `data:` URL, which the browser decodes. Nothing
//! here decodes an image.

use std::collections::BTreeMap;

use clackatoo_protocol::{
    ImageAnswer, ImageFormat, ImageFrom, ImageId, ImageInfo, Request, TerminalId, dropped_words,
};

use super::{Asked, Online, State};

/// The size an image is asked for at, in pixels: no wider or taller.
pub type Fit = [u32; 2];

/// What a fit is rounded up to a multiple of, in pixels, so images shown
/// at about one size share one copy.
const FIT_STEP: u32 = 64;

/// How long a copy being made is waited for, in milliseconds, before it's
/// asked for again: the word that it's ready may not come through a
/// daemon passing requests on.
const SCALING_AGAIN: f64 = 1000.0;

/// The most bytes of `data:` URLs kept: those used longest ago go first.
const MOST_URLS: usize = 32 * 1024 * 1024;

/// The most bytes one image may be: one larger is shown as missing.
const MOST_IMAGE: u64 = 16 * 1024 * 1024;

/// How an image being fetched is going.
#[derive(Debug)]
enum Fetch {
    /// Not asked for yet: the connection isn't open, or the daemon's run
    /// isn't known yet.
    Waiting,
    /// Asked for, with the bytes come so far.
    Coming(Vec<u8>),
    /// A copy is being made, asked for again at this time, in
    /// milliseconds, unless the daemon says first it's ready.
    Scaling(f64),
}

/// The images the app has, or is fetching.
#[derive(Debug, Default)]
pub(super) struct Layer {
    /// Those being fetched, by image and fit, with the chat they're of.
    fetching: BTreeMap<(ImageId, Fit), (TerminalId, Fetch)>,
    /// Those come, as `data:` URLs, with when each was last used.
    urls: BTreeMap<(ImageId, Fit), (String, u64)>,
    /// The bytes of `urls`.
    bytes: usize,
    /// Counts uses, to know which URL was used longest ago.
    uses: u64,
    /// The fit each image was last asked at, which it's shown at.
    fits: BTreeMap<ImageId, Fit>,
    /// Those that won't come: the words in their place if they were
    /// dropped, `None` if they're missing.
    gone: BTreeMap<ImageId, Option<String>>,
}

/// How an image is to be shown, once that's known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look<'a> {
    /// From this `data:` URL.
    Url(&'a str),
    /// As these words: it was dropped.
    Dropped(&'a str),
    /// It's missing: as what the page says of it.
    Missing,
}

/// The fit to ask for an image `css` CSS pixels in size at, on a screen
/// of `ratio` pixels to a CSS pixel: each side rounded up to a multiple
/// of [`FIT_STEP`]. `None` if it isn't laid out, as when folded away.
#[must_use]
pub fn fit_of(css: [f64; 2], ratio: f64) -> Option<Fit> {
    if !(css[0] > 0.0 && css[1] > 0.0) {
        return None;
    }
    let most = f64::from(ImageInfo::MOST_SIDE / FIT_STEP);
    let side = |css: f64| {
        let steps = (css * ratio.max(1.0) / f64::from(FIT_STEP))
            .ceil()
            .clamp(1.0, most);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to a few hundred steps"
        )]
        let steps = steps as u32;
        steps * FIT_STEP
    };
    Some([side(css[0]), side(css[1])])
}

/// The image `hex` names, as an element's `data-image` holds it.
#[must_use]
pub fn image_id(hex: &str) -> Option<ImageId> {
    let mut id = [0; 16];
    if hex.len() != 32 {
        return None;
    }
    for (at, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(at * 2..at * 2 + 2)?, 16).ok()?;
    }
    Some(ImageId(id))
}

/// `bytes`, encoded as `format`, as a `data:` URL.
fn data_url(format: ImageFormat, bytes: &[u8]) -> String {
    use base64::Engine as _;
    let mut url = format!("data:{};base64,", format.media_type());
    base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut url);
    url
}

impl State {
    /// The image `image` of chat `chat` is on the screen, to be shown at
    /// `fit`: it's asked for, unless it's here, coming or gone.
    pub fn want_image(&mut self, chat: TerminalId, image: ImageId, fit: Fit) {
        let layer = &mut self.layer;
        if layer.gone.contains_key(&image) {
            self.dirty.images = true;
            return;
        }
        layer.fits.insert(image, fit);
        let key = (image, fit);
        if layer.urls.contains_key(&key) {
            self.dirty.images = true;
            return;
        }
        if layer.fetching.contains_key(&key) {
            return;
        }
        layer.fetching.insert(key, (chat, Fetch::Waiting));
        self.ask_image(key, 0);
    }

    /// How image `image` is to be shown, at the fit it was last asked at,
    /// once that's known.
    pub fn image_look(&mut self, image: ImageId) -> Option<Look<'_>> {
        let layer = &mut self.layer;
        if let Some(gone) = layer.gone.get(&image) {
            return Some(gone.as_deref().map_or(Look::Missing, Look::Dropped));
        }
        let fit = *layer.fits.get(&image)?;
        let (url, used) = layer.urls.get_mut(&(image, fit))?;
        layer.uses += 1;
        *used = layer.uses;
        Some(Look::Url(url))
    }

    /// When an image whose copy is being made is next asked for again, in
    /// milliseconds, if one is.
    #[must_use]
    pub fn images_again_at(&self) -> Option<f64> {
        (self.layer.fetching.values())
            .filter_map(|(_, fetch)| match fetch {
                Fetch::Scaling(at) => Some(*at),
                _ => None,
            })
            .reduce(f64::min)
    }

    /// Asks again for the images whose copies have been waited for long
    /// enough.
    pub fn images_again(&mut self) {
        let now = self.now;
        let due: Vec<_> = (self.layer.fetching.iter())
            .filter(|(_, (_, fetch))| matches!(fetch, Fetch::Scaling(at) if *at <= now))
            .map(|(key, _)| *key)
            .collect();
        for key in due {
            self.ask_image(key, 0);
        }
    }

    /// Asks for image `key`'s bytes from `at`, if requests may go: else
    /// it waits until they may.
    fn ask_image(&mut self, key: (ImageId, Fit), at: u64) {
        let ready = self.online == Online::Open && self.run_confirmed;
        let Some((chat, fetch)) = self.layer.fetching.get_mut(&key) else {
            return;
        };
        if !ready {
            *fetch = Fetch::Waiting;
            return;
        }
        if at == 0 {
            *fetch = Fetch::Coming(Vec::new());
        }
        let (image, fit) = key;
        let request = Request::Image {
            from: ImageFrom::Terminal(*chat),
            image,
            fit: Some(fit),
            at,
        };
        self.request(request, Asked::Image { image, fit });
    }

    /// The connection opened: what was asked for before is asked again
    /// once the daemon's run is known.
    pub(super) fn images_unasked(&mut self) {
        for (_, fetch) in self.layer.fetching.values_mut() {
            *fetch = Fetch::Waiting;
        }
    }

    /// The daemon's run is known: asks for what waited.
    pub(super) fn images_ask_waiting(&mut self) {
        let waiting: Vec<_> = (self.layer.fetching.iter())
            .filter(|(_, (_, fetch))| matches!(fetch, Fetch::Waiting))
            .map(|(key, _)| *key)
            .collect();
        for key in waiting {
            self.ask_image(key, 0);
        }
    }

    /// The daemon started afresh: what's fetched is of chats it numbers
    /// otherwise now. What came is kept, as an image's ID is its bytes'.
    pub(super) fn images_let_go(&mut self) {
        self.layer.fetching.clear();
    }

    /// A copy of `image` at `fit` is ready: it's asked for again now.
    pub(super) fn image_scaled(&mut self, image: ImageId, fit: Fit) {
        if let Some((_, Fetch::Scaling(_))) = self.layer.fetching.get(&(image, fit)) {
            self.ask_image((image, fit), 0);
        }
    }

    /// Takes in `answer`, to the request for `image` at `fit`.
    pub(super) fn image_answered(&mut self, image: ImageId, fit: Fit, answer: ImageAnswer) {
        let key = (image, fit);
        let Some((_, Fetch::Coming(have))) = self.layer.fetching.get_mut(&key) else {
            // Let go meanwhile.
            return;
        };
        match answer {
            ImageAnswer::Part {
                info,
                total,
                at,
                bytes,
            } if at == have.len() as u64
                && total <= MOST_IMAGE
                && at + bytes.len() as u64 <= total
                && (!bytes.is_empty() || at == total) =>
            {
                have.extend_from_slice(&bytes);
                let got = have.len() as u64;
                if got < total {
                    self.ask_image(key, got);
                    return;
                }
                let url = data_url(info.format, have);
                self.layer.fetching.remove(&key);
                self.image_came(key, url);
            }
            ImageAnswer::Scaling => {
                if let Some((_, fetch)) = self.layer.fetching.get_mut(&key) {
                    *fetch = Fetch::Scaling(self.now + SCALING_AGAIN);
                }
                return;
            }
            ImageAnswer::Dropped { limit } => self.image_gone(key, Some(dropped_words(limit))),
            _ => self.image_gone(key, None),
        }
        self.dirty.images = true;
    }

    /// The request for `image` at `fit` failed: it's missing.
    pub(super) fn image_failed(&mut self, image: ImageId, fit: Fit) {
        if self.layer.fetching.contains_key(&(image, fit)) {
            self.image_gone((image, fit), None);
            self.dirty.images = true;
        }
    }

    /// Image `key` won't come: `words` in its place, if any.
    fn image_gone(&mut self, key: (ImageId, Fit), words: Option<String>) {
        self.layer.fetching.remove(&key);
        self.layer.gone.insert(key.0, words);
    }

    /// Image `key` came, as `url`: kept, letting go of those used longest
    /// ago while there are too many.
    fn image_came(&mut self, key: (ImageId, Fit), url: String) {
        let layer = &mut self.layer;
        layer.uses += 1;
        layer.bytes += url.len();
        if let Some((old, _)) = layer.urls.insert(key, (url, layer.uses)) {
            layer.bytes -= old.len();
        }
        while layer.bytes > MOST_URLS && layer.urls.len() > 1 {
            let Some(oldest) = (layer.urls.iter())
                .filter(|(other, _)| **other != key)
                .min_by_key(|(_, (_, used))| *used)
                .map(|(other, _)| *other)
            else {
                break;
            };
            if let Some((old, _)) = layer.urls.remove(&oldest) {
                layer.bytes -= old.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clackatoo_protocol::{ClientMessage, Event, ImageNews, Response, ServerMessage};

    const CHAT: TerminalId = TerminalId(4);
    const IMAGE: ImageId = ImageId([7; 16]);

    fn state() -> State {
        let mut state = State::default();
        state.connected_now();
        state.asked.clear();
        state.take_outbox();
        state
    }

    fn info() -> ImageInfo {
        ImageInfo {
            id: IMAGE,
            width: 2,
            height: 1,
            format: ImageFormat::Jpeg,
            bytes: 5,
        }
    }

    fn asked(state: &mut State) -> Vec<(Option<Fit>, u64)> {
        (state.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::Image {
                    from: ImageFrom::Terminal(CHAT),
                    image: IMAGE,
                    fit,
                    at,
                }) => Some((fit, at)),
                _ => None,
            })
            .collect()
    }

    fn answer(state: &mut State, answer: ImageAnswer) {
        state.receive(ServerMessage::Response(Response::Image(answer)));
    }

    #[test]
    fn fits_round_up_to_shared_steps() {
        assert_eq!(fit_of([100.0, 40.5], 2.0), Some([256, 128]));
        assert_eq!(fit_of([64.0, 1.0], 1.0), Some([64, 64]));
        assert_eq!(fit_of([0.0, 40.0], 3.0), None, "not laid out");
        assert_eq!(fit_of([1e9, 10.0], 1.0), Some([ImageInfo::MOST_SIDE, 64]));
    }

    #[test]
    fn image_ids_are_read_from_their_hex() {
        assert_eq!(image_id(&IMAGE.to_string()), Some(IMAGE));
        assert_eq!(image_id("07"), None);
        assert_eq!(image_id(&"zz".repeat(16)), None);
    }

    #[test]
    fn data_urls_are_standard_base64() {
        assert_eq!(data_url(ImageFormat::Png, b""), "data:image/png;base64,");
        assert_eq!(
            data_url(ImageFormat::Png, b"f"),
            "data:image/png;base64,Zg=="
        );
        assert_eq!(
            data_url(ImageFormat::Gif, b"fo"),
            "data:image/gif;base64,Zm8="
        );
        assert_eq!(
            data_url(ImageFormat::Webp, b"foob\xff\xfe"),
            "data:image/webp;base64,Zm9vYv/+"
        );
    }

    #[test]
    fn an_image_comes_in_parts_as_a_data_url() {
        let mut s = state();
        let fit = fit_of([100.0, 50.0], 2.0).unwrap();
        s.want_image(CHAT, IMAGE, fit);
        assert_eq!(asked(&mut s), [(Some([256, 128]), 0)]);
        // Asked once, however often it's seen.
        s.want_image(CHAT, IMAGE, fit);
        assert!(asked(&mut s).is_empty());
        assert_eq!(s.image_look(IMAGE), None);
        let part = |at: u64, bytes: &[u8]| ImageAnswer::Part {
            info: info(),
            total: 5,
            at,
            bytes: bytes.to_vec(),
        };
        answer(&mut s, part(0, b"foo"));
        assert_eq!(asked(&mut s), [(Some(fit), 3)]);
        assert_eq!(s.image_look(IMAGE), None);
        answer(&mut s, part(3, b"ba"));
        assert!(asked(&mut s).is_empty());
        assert!(s.dirty.images);
        assert_eq!(
            s.image_look(IMAGE),
            Some(Look::Url("data:image/jpeg;base64,Zm9vYmE="))
        );
        // Seen again, it's here.
        s.want_image(CHAT, IMAGE, fit);
        assert!(asked(&mut s).is_empty());
    }

    #[test]
    fn a_copy_being_made_is_asked_for_again() {
        let mut s = state();
        s.now = 100.0;
        s.want_image(CHAT, IMAGE, [64, 64]);
        asked(&mut s);
        answer(&mut s, ImageAnswer::Scaling);
        assert_eq!(s.images_again_at(), Some(100.0 + SCALING_AGAIN));
        s.images_again();
        assert!(asked(&mut s).is_empty(), "not yet");
        // The daemon says it's ready.
        s.receive(ServerMessage::Event(Event::Image(ImageNews::Scaled {
            from: ImageFrom::Terminal(CHAT),
            image: IMAGE,
            fit: [64, 64],
        })));
        assert_eq!(asked(&mut s), [(Some([64, 64]), 0)]);
        assert_eq!(s.images_again_at(), None);
        // Or, through a daemon that never says, after a while.
        answer(&mut s, ImageAnswer::Scaling);
        s.now += SCALING_AGAIN;
        s.images_again();
        assert_eq!(asked(&mut s), [(Some([64, 64]), 0)]);
    }

    #[test]
    fn dropped_and_missing_images_say_so() {
        let mut s = state();
        s.want_image(CHAT, IMAGE, [64, 64]);
        answer(&mut s, ImageAnswer::Dropped { limit: 64 << 20 });
        assert_eq!(
            s.image_look(IMAGE),
            Some(Look::Dropped("[image dropped: over the 64 MB limit]"))
        );
        let other = ImageId([9; 16]);
        s.want_image(CHAT, other, [64, 64]);
        s.receive(ServerMessage::Response(Response::Error(
            clackatoo_protocol::Error::NoSuchTerminal(CHAT),
        )));
        assert_eq!(s.image_look(other), Some(Look::Missing));
        // Not asked for again.
        s.take_outbox();
        s.want_image(CHAT, other, [128, 64]);
        assert!(s.take_outbox().is_empty());
    }

    #[test]
    fn images_wait_for_the_connection_and_ask_again_on_it() {
        let mut s = state();
        s.want_image(CHAT, IMAGE, [64, 64]);
        asked(&mut s);
        s.lost(1);
        s.connected();
        assert!(asked(&mut s).is_empty(), "not before the run is known");
        s.connected_now();
        assert_eq!(asked(&mut s), [(Some([64, 64]), 0)]);
    }

    #[test]
    fn urls_kept_are_bounded() {
        let mut s = state();
        let big = vec![0; MOST_URLS / 3];
        for n in 0..4u8 {
            let image = ImageId([n; 16]);
            s.image_came((image, [64, 64]), data_url(ImageFormat::Png, &big));
        }
        assert!(s.layer.bytes <= MOST_URLS);
        assert!(!s.layer.urls.contains_key(&(ImageId([0; 16]), [64, 64])));
        assert!(s.layer.urls.contains_key(&(ImageId([3; 16]), [64, 64])));
    }
}
