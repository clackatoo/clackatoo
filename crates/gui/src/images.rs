//! The window's side of the image layer (CLACKA-239): every image a pane
//! shows that the daemon keeps, a chat's, a picture pane's or a Markdown
//! page's, is fetched once by its ID, decoded on a thread of its own, and
//! drawn from one cache, shared by the process's windows.
//!
//! Panes don't fetch anything themselves. As they're painted, they ask
//! [`Images::show`] for an image at the size they draw it, and say whose
//! it is; what isn't ready yet is noted as wanted, and the desk fetches it
//! and has it decoded as it settles (`desk/images.rs`), drawing again once
//! it's come. A window on this machine fetches an image as it was sent; one
//! at the far end of a link, a copy no bigger than it's drawn.
//!
//! What's kept is bounded, each part by bytes, the image used longest ago
//! let go first: the bytes fetched, the pixels decoded, and the textures on
//! the GPU (see `render::textures`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use clackatoo_protocol::{ImageFormat, ImageFrom, ImageId, ImageInfo};

use crate::render::{Image, MAX_IMAGE_SIDE, TextureKey};

/// The sides images are decoded at: one drawn at any size is decoded at
/// the next of these up, or as it is, if it's smaller.
const SIDES: [u32; 8] = [64, 128, 256, 512, 1024, 2048, 4096, MAX_IMAGE_SIDE];

/// How many bytes of images fetched are kept, as the daemon sent them.
const BYTES_KEPT: u64 = 128 * 1024 * 1024;

/// How many bytes of decoded pixels are kept.
const PIXELS_KEPT: u64 = 256 * 1024 * 1024;

/// The number the next picture decoded here gets, never reused, so the
/// renderer uploads each once.
static NEXT_PICTURE: AtomicU64 = AtomicU64::new(1 << 60);

/// The side an image drawn `drawn` pixels at its longer side is decoded
/// at.
#[must_use]
pub(crate) fn side_for(drawn: f32) -> u32 {
    SIDES
        .iter()
        .copied()
        .find(|&side| f64::from(side) >= f64::from(drawn))
        .unwrap_or(MAX_IMAGE_SIDE)
}

/// How an image a pane wants to draw is.
#[derive(Clone, Debug)]
pub(crate) enum Shown {
    /// Ready: its pixels, and what the GPU keeps them by. They may be at
    /// another size than asked for, while that one comes.
    Ready(Arc<Image>, TextureKey),
    /// On its way.
    Loading,
    /// The daemon let it go to keep within this many bytes: said as such
    /// in its place.
    Dropped(u64),
    /// It can't be shown, and why.
    Failed(String),
}

/// Why an image can't be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Gone {
    Dropped(u64),
    Failed(String),
}

/// An image's bytes, fetched.
#[derive(Debug)]
struct Fetched {
    /// What they are: the image, or a copy, whose size this says.
    info: ImageInfo,
    bytes: Arc<[u8]>,
    used: u64,
}

/// An image decoded.
#[derive(Debug)]
struct Decoded {
    image: Arc<Image>,
    used: u64,
}

/// What the decoding thread is asked: `bytes`, which `info` describes,
/// decoded at no more than `side`.
struct Job {
    key: (ImageId, u32),
    info: ImageInfo,
    bytes: Arc<[u8]>,
}

/// The images fetched and decoded, shared by the process's windows.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    /// The bytes fetched, by image, and the side of the copy, if it is one.
    fetched: HashMap<(ImageId, Option<u32>), Fetched>,
    fetched_bytes: u64,
    decoded: HashMap<(ImageId, u32), Decoded>,
    decoded_bytes: u64,
    gone: HashMap<ImageId, Gone>,
    decoding: HashSet<(ImageId, u32)>,
    worker: Option<Worker>,
    tick: u64,
}

/// The decoding thread, started when it's first needed.
#[derive(Debug)]
struct Worker {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<((ImageId, u32), Result<Image, String>)>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("key", &self.key)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// The cache, shared.
pub(crate) type Shared = Arc<Mutex<Cache>>;

/// A cache of its own, as for a window alone.
#[must_use]
pub(crate) fn shared() -> Shared {
    Arc::new(Mutex::new(Cache::default()))
}

fn lock(shared: &Shared) -> MutexGuard<'_, Cache> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Cache {
    /// How image `id`, wanted at `side`, is: the nearest size decoded
    /// stands in for it meanwhile.
    fn shown(&mut self, id: ImageId, side: u32) -> Option<Shown> {
        self.tick += 1;
        let tick = self.tick;
        if let Some(decoded) = self.decoded.get_mut(&(id, side)) {
            decoded.used = tick;
            return Some(Shown::Ready(
                Arc::clone(&decoded.image),
                TextureKey::Image(id, side),
            ));
        }
        if let Some(gone) = self.gone.get(&id) {
            return Some(match gone {
                Gone::Dropped(limit) => Shown::Dropped(*limit),
                Gone::Failed(why) => Shown::Failed(why.clone()),
            });
        }
        let nearest = (self.decoded.iter_mut())
            .filter(|((image, _), _)| *image == id)
            .min_by_key(|((_, other), _)| other.abs_diff(side));
        nearest.map(|(&(_, other), decoded)| {
            decoded.used = tick;
            Shown::Ready(Arc::clone(&decoded.image), TextureKey::Image(id, other))
        })
    }

    /// Whether image `id` at `side` is ready, or can't be shown.
    fn settled(&self, id: ImageId, side: u32) -> bool {
        self.decoded.contains_key(&(id, side)) || self.gone.contains_key(&id)
    }

    /// The bytes fetched of image `id` that make it at `side`: the image
    /// itself, or a copy at least that big.
    fn bytes_for(&self, id: ImageId, side: u32) -> Option<(ImageInfo, Arc<[u8]>)> {
        (self.fetched.iter())
            .filter(|((image, copy), _)| *image == id && copy.is_none_or(|copy| copy >= side))
            .min_by_key(|((_, copy), _)| copy.unwrap_or(u32::MAX))
            .map(|(_, fetched)| (fetched.info, Arc::clone(&fetched.bytes)))
    }

    /// Keeps the bytes fetched of image `id`, or of its copy at `copy`.
    pub(crate) fn keep(&mut self, id: ImageId, copy: Option<u32>, info: ImageInfo, bytes: Vec<u8>) {
        self.tick += 1;
        self.gone.remove(&id);
        self.fetched_bytes += bytes.len() as u64;
        let fetched = Fetched {
            info,
            bytes: bytes.into(),
            used: self.tick,
        };
        if let Some(old) = self.fetched.insert((id, copy), fetched) {
            self.fetched_bytes -= old.bytes.len() as u64;
        }
        while self.fetched_bytes > BYTES_KEPT {
            let Some(oldest) = (self.fetched.iter())
                .min_by_key(|(_, fetched)| fetched.used)
                .map(|(&key, _)| key)
            else {
                break;
            };
            if let Some(fetched) = self.fetched.remove(&oldest) {
                self.fetched_bytes -= fetched.bytes.len() as u64;
            }
        }
    }

    /// Notes image `id` can't be shown, as `gone` says.
    pub(crate) fn gone(&mut self, id: ImageId, gone: Gone) {
        self.gone.insert(id, gone);
    }

    /// Has image `id`'s bytes decoded at `side`, if they're fetched and
    /// it isn't being already. Returns whether it is now.
    fn decode(&mut self, id: ImageId, side: u32) -> bool {
        if self.decoding.contains(&(id, side)) {
            return true;
        }
        let Some((info, bytes)) = self.bytes_for(id, side) else {
            return false;
        };
        if self.worker.is_none() {
            let (jobs, receive) = mpsc::channel::<Job>();
            let (send, done) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("images".into())
                .spawn(move || {
                    while let Ok(job) = receive.recv() {
                        let side = job.key.1;
                        let decoded = decode(job.info, &job.bytes, side);
                        if send.send((job.key, decoded)).is_err() {
                            break;
                        }
                    }
                });
            if let Err(err) = spawned {
                self.gone(id, Gone::Failed(format!("it can't be decoded: {err}")));
                return false;
            }
            self.worker = Some(Worker { jobs, done });
        }
        let worker = self.worker.as_ref().expect("started above");
        let job = Job {
            key: (id, side),
            info,
            bytes,
        };
        if worker.jobs.send(job).is_err() {
            return false;
        }
        self.decoding.insert((id, side));
        true
    }

    /// Takes in what the decoding thread has done. Returns whether
    /// anything came.
    fn take_decoded(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let done: Vec<_> = worker.done.try_iter().collect();
        let came = !done.is_empty();
        for (key, decoded) in done {
            self.decoding.remove(&key);
            match decoded {
                Ok(image) => {
                    self.tick += 1;
                    self.decoded_bytes += image.rgba.len() as u64;
                    let decoded = Decoded {
                        image: Arc::new(image),
                        used: self.tick,
                    };
                    if let Some(old) = self.decoded.insert(key, decoded) {
                        self.decoded_bytes -= old.image.rgba.len() as u64;
                    }
                }
                Err(why) => self.gone(key.0, Gone::Failed(why)),
            }
        }
        while self.decoded_bytes > PIXELS_KEPT {
            let Some(oldest) = (self.decoded.iter())
                .min_by_key(|(_, decoded)| decoded.used)
                .map(|(&key, _)| key)
            else {
                break;
            };
            if let Some(decoded) = self.decoded.remove(&oldest) {
                self.decoded_bytes -= decoded.image.rgba.len() as u64;
            }
        }
        came
    }

    /// Whether anything's being decoded.
    fn decoding(&self) -> bool {
        !self.decoding.is_empty()
    }
}

/// `bytes`, the image `info` describes, decoded, no wider or taller than
/// `side`, or why not.
fn decode(info: ImageInfo, bytes: &[u8], side: u32) -> Result<Image, String> {
    if !ImageInfo::sized(info.width, info.height) {
        return Err("it's too big to show".into());
    }
    let format = match info.format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(ImageInfo::MOST_PIXELS * 4 + 64 * 1024 * 1024);
    limits.max_image_width = Some(ImageInfo::MOST_SIDE);
    limits.max_image_height = Some(ImageInfo::MOST_SIDE);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| "it isn't an image that can be read".to_owned())?;
    let side = side.min(MAX_IMAGE_SIDE);
    let decoded = if decoded.width() > side || decoded.height() > side {
        decoded.thumbnail(side, side)
    } else {
        decoded
    };
    let rgba = decoded.to_rgba8();
    Ok(Image {
        id: NEXT_PICTURE.fetch_add(1, Ordering::Relaxed),
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// A window's way into the cache, which panes ask as they're painted.
#[derive(Clone, Debug)]
pub(crate) struct Images {
    shared: Shared,
    /// The images panes asked for and didn't have: each at a side, with
    /// whose it is.
    wanted: Rc<RefCell<HashMap<(ImageId, u32), ImageFrom>>>,
}

impl Default for Images {
    fn default() -> Self {
        Self::new(shared())
    }
}

impl Images {
    /// A way into `shared`.
    pub(crate) fn new(shared: Shared) -> Self {
        Self {
            shared,
            wanted: Rc::default(),
        }
    }

    /// How image `id`, `from`'s, drawn `drawn` pixels at its longer side,
    /// is: noted as wanted if it isn't ready at that size.
    pub(crate) fn show(&self, from: ImageFrom, id: ImageId, drawn: f32) -> Shown {
        let side = side_for(drawn);
        let mut cache = lock(&self.shared);
        let shown = cache.shown(id, side);
        if !cache.settled(id, side) {
            self.wanted.borrow_mut().insert((id, side), from);
        }
        shown.unwrap_or(Shown::Loading)
    }

    /// Takes the images wanted since this was last asked, and whose each
    /// is: those ready, or that can't be, left out.
    pub(crate) fn take_wanted(&self) -> Vec<((ImageId, u32), ImageFrom)> {
        let wanted = std::mem::take(&mut *self.wanted.borrow_mut());
        let cache = lock(&self.shared);
        (wanted.into_iter())
            .filter(|((id, side), _)| !cache.settled(*id, *side))
            .collect()
    }

    /// Has image `id` decoded at `side`, if its bytes are fetched. Returns
    /// whether it's being decoded now.
    pub(crate) fn decode(&self, id: ImageId, side: u32) -> bool {
        lock(&self.shared).decode(id, side)
    }

    /// Keeps the bytes fetched of image `id`, or of its copy at `copy`.
    pub(crate) fn keep(&self, id: ImageId, copy: Option<u32>, info: ImageInfo, bytes: Vec<u8>) {
        lock(&self.shared).keep(id, copy, info, bytes);
    }

    /// Notes image `id` can't be shown, as `gone` says.
    pub(crate) fn gone(&self, id: ImageId, gone: Gone) {
        lock(&self.shared).gone(id, gone);
    }

    /// Takes in what the decoding thread has done. Returns whether
    /// anything came, and whether anything's still being decoded.
    pub(crate) fn take_decoded(&self) -> (bool, bool) {
        let mut cache = lock(&self.shared);
        let came = cache.take_decoded();
        (came, cache.decoding())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba(rgba))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn info(id: u8, width: u32, height: u32, bytes: &[u8]) -> ImageInfo {
        ImageInfo {
            id: ImageId([id; 16]),
            width,
            height,
            format: ImageFormat::Png,
            bytes: bytes.len() as u64,
        }
    }

    /// Waits for the decoding thread to finish.
    fn settle(images: &Images) {
        for _ in 0..500 {
            let (_, decoding) = images.take_decoded();
            if !decoding {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("decoding never finished");
    }

    #[test]
    fn an_image_wanted_is_decoded_once_fetched_at_the_side_drawn() {
        let images = Images::default();
        let from = ImageFrom::Terminal(clackatoo_protocol::TerminalId(1));
        let bytes = png(1000, 500, [10, 20, 30, 255]);
        let info = info(1, 1000, 500, &bytes);
        assert!(matches!(images.show(from, info.id, 200.0), Shown::Loading));
        assert_eq!(images.take_wanted(), vec![((info.id, 256), from)]);
        assert!(!images.decode(info.id, 256), "nothing fetched yet");
        images.keep(info.id, None, info, bytes);
        assert!(images.decode(info.id, 256));
        settle(&images);
        let Shown::Ready(image, key) = images.show(from, info.id, 200.0) else {
            panic!("not ready");
        };
        assert_eq!((image.width, image.height), (256, 128));
        assert_eq!(&image.rgba[..4], &[10, 20, 30, 255]);
        assert_eq!(key, TextureKey::Image(info.id, 256));
        assert!(images.take_wanted().is_empty());
        // Wanted bigger, the smaller stands in while it's decoded.
        let Shown::Ready(standing, key) = images.show(from, info.id, 900.0) else {
            panic!("nothing stands in");
        };
        assert_eq!(standing.width, 256);
        assert_eq!(key, TextureKey::Image(info.id, 256));
        assert_eq!(images.take_wanted(), vec![((info.id, 1024), from)]);
        assert!(images.decode(info.id, 1024));
        settle(&images);
        let Shown::Ready(whole, _) = images.show(from, info.id, 900.0) else {
            panic!("not ready");
        };
        assert_eq!(
            (whole.width, whole.height),
            (1000, 500),
            "never made bigger"
        );
    }

    #[test]
    fn what_cant_be_shown_says_why_and_isnt_wanted_again() {
        let images = Images::default();
        let from = ImageFrom::Picture(clackatoo_protocol::PictureId(1));
        let bad = b"not a png".to_vec();
        let info = info(2, 10, 10, &bad);
        images.keep(info.id, None, info, bad);
        assert!(images.decode(info.id, 64));
        settle(&images);
        assert!(matches!(images.show(from, info.id, 10.0), Shown::Failed(_)));
        assert!(images.take_wanted().is_empty());
        let dropped = ImageId([3; 16]);
        images.gone(dropped, Gone::Dropped(64 << 20));
        assert!(
            matches!(images.show(from, dropped, 10.0), Shown::Dropped(limit) if limit == 64 << 20)
        );
    }

    #[test]
    fn sides_go_up_in_steps_and_stop_at_the_largest_texture() {
        assert_eq!(side_for(0.0), 64);
        assert_eq!(side_for(64.0), 64);
        assert_eq!(side_for(65.0), 128);
        assert_eq!(side_for(100_000.0), MAX_IMAGE_SIDE);
    }
}
