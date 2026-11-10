//! Images in a terminal (CLACKA-239): kitty's graphics protocol (`APC G`),
//! its Unicode placeholders, sixel (`DCS q`) and iTerm2's inline images
//! (`OSC 1337 ; File=`), all made into one model.
//!
//! # Pictures and pieces
//!
//! What a program shows is a [`Picture`]: all or part of an image, over a
//! rectangle of cells, above or below the text. Each row the picture
//! covers holds a [`Piece`] of it, saying which picture and which of its
//! rows, so a picture scrolls with the text, into history and out of it,
//! and goes when its rows do, as everything else a row holds does. Text
//! printed over a picture doesn't take it away, as in kitty; clearing the
//! screen does, and so does erasing a row's text, but not its cells.
//!
//! Pictures are numbered as links are (see [`crate::links`]): never twice
//! while the terminal lives, so a window can keep what it's told of each.
//! Those no row shows any more are forgotten from time to time.
//!
//! kitty's Unicode placeholders are text a program prints (see
//! [`placeholder`]), drawn by a window from the terminal's virtual
//! placements ([`Virtual`]).
//!
//! # Images
//!
//! The terminal doesn't decode, hash or keep images: its owner does, off
//! the thread the terminal lives on. Once an image's data is all here, the
//! terminal asks for it to be kept ([`ImageRequest`]), and waits: it reads
//! nothing more until its owner says what came of it
//! ([`Terminal::image_kept`](crate::Terminal::image_kept)), as where the
//! cursor goes, and what's answered, depends on the image's size. An image
//! is known by its [`ImageKey`] from then on; the terminal says which it
//! no longer needs ([`Terminal::take_released_images`]).
//!
//! # Answers
//!
//! kitty's answers are made of fixed words and numbers parsed here, never
//! anything a program sent (see `safe_reply`).
//!
//! [`Terminal::take_released_images`]: crate::Terminal::take_released_images

mod iterm;
mod kitty;
pub mod placeholder;
pub mod sixel;
#[cfg(test)]
mod tests;

use std::collections::{BTreeSet, HashMap, HashSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT as BASE64;

pub use placeholder::{PLACEHOLDER, Placeholder, has_placeholders, placeholders};

use crate::grid::Grid;
use crate::terminal::Emulator;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// An image, by the ID its owner keeps it by: the first 16 bytes of the
/// SHA-256 of its bytes as kept.
pub type ImageKey = [u8; 16];

/// How many parts a cell's width and height are split into, where a
/// picture's size and offset are said in them.
pub const PARTS: u32 = 64;

/// How big a cell is taken to be, in pixels, until a window says.
const DEFAULT_CELL: [u32; 2] = [10, 20];

/// The most pictures a terminal keeps: past this, those no row shows are
/// forgotten, and then, if it's still past it, more aren't shown.
const MOST_PICTURES: usize = 4096;

/// The most images kitty's commands may keep by ID, shown or not: past
/// this, the one transmitted longest ago is let go.
const MOST_IMAGES: usize = 1024;

/// The most virtual placements, for Unicode placeholders.
const MOST_VIRTUALS: usize = 256;

/// The most columns and rows one picture covers.
const MOST_CELLS: u32 = 1024;

/// The most bytes of images a terminal holds, by default: what one image
/// may be, too, before it's refused.
pub const DEFAULT_LIMIT: u64 = 64 * 1024 * 1024;

/// The most bytes of a kitty command's payload that names a file.
const LONGEST_NAME: usize = 4096;

/// The words in place of an image `width` by `height` pixels, in text
/// copied or read from a terminal: its size, never its bytes.
#[must_use]
pub fn words(width: u32, height: u32) -> String {
    format!("[image {width}×{height}]")
}

/// A picture a terminal shows: all or part of an image, over a rectangle
/// of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Picture {
    /// The image.
    pub image: ImageKey,
    /// The image's width and height, in pixels.
    pub size: [u32; 2],
    /// The part of it shown: left, top, width and height, in pixels.
    pub crop: [u32; 4],
    /// How many columns and rows it covers.
    pub cells: [u16; 2],
    /// Where it starts in its first cell, in [`PARTS`] of a cell.
    pub offset: [u16; 2],
    /// How wide and tall it's drawn, in [`PARTS`] of a cell: the part
    /// shown is stretched to this.
    pub span: [u32; 2],
    /// Its place among the text and other pictures: below the text if
    /// negative, above it otherwise.
    pub z: i32,
}

/// One row's piece of a picture: row `row` of picture `picture`, from
/// column `col`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Piece {
    /// The picture's number.
    pub picture: u32,
    /// The column its left edge is in.
    pub col: u16,
    /// Which of its rows this is.
    pub row: u16,
}

/// A virtual placement: an image placed for kitty's Unicode placeholders
/// to show (see [`placeholder`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Virtual {
    /// The image's ID, as the program gave it.
    pub id: u32,
    /// The placement's ID, as the program gave it.
    pub placement: u32,
    /// The image.
    pub image: ImageKey,
    /// The image's width and height, in pixels.
    pub size: [u32; 2],
    /// The columns and rows of placeholders it's fitted to.
    pub cells: [u16; 2],
}

/// Where part of an image is drawn, and which part.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    /// The image.
    pub image: ImageKey,
    /// Its width and height, in pixels.
    pub size: [u32; 2],
    /// Where it's drawn, left, top, width and height, in cells: for a
    /// [`Picture`]'s piece from the left of its row and the top of it,
    /// for a placeholder from the left and top of its cell.
    pub rect: [f32; 4],
    /// The part of the image drawn there, left, top, width and height, in
    /// its pixels.
    pub crop: [f32; 4],
    /// Below the text if negative, above it otherwise.
    pub z: i32,
}

impl Picture {
    /// What `piece`, one of this picture's, draws of it on its row.
    #[must_use]
    pub fn slice(&self, piece: Piece) -> Option<Slice> {
        #[allow(clippy::cast_precision_loss, reason = "sizes far below 2^24")]
        let parts = |value: u32| value as f32 / PARTS as f32;
        let (width, height) = (parts(self.span[0]), parts(self.span[1]));
        // The picture's top, from the top of the piece's row.
        let top = parts(u32::from(self.offset[1])) - f32::from(piece.row);
        let (from, to) = (top.max(0.0), (top + height).min(1.0));
        if width <= 0.0 || to <= from {
            return None;
        }
        #[allow(clippy::cast_precision_loss, reason = "sizes far below 2^24")]
        let [x, y, w, h] = self.crop.map(|value| value as f32);
        Some(Slice {
            image: self.image,
            size: self.size,
            rect: [
                f32::from(piece.col) + parts(u32::from(self.offset[0])),
                from,
                width,
                to - from,
            ],
            crop: [
                x,
                y + (from - top) / height * h,
                w,
                (to - from) / height * h,
            ],
            z: self.z,
        })
    }
}

impl Virtual {
    /// What the placeholder at `row` and `col` of this placement draws of
    /// its image, in cells `cell` pixels big: the image fitted to the
    /// placement's cells and centred, as kitty draws it.
    #[must_use]
    pub fn slice(&self, row: u16, col: u16, cell: [f32; 2]) -> Option<Slice> {
        #[allow(clippy::cast_precision_loss, reason = "sizes far below 2^24")]
        let [width, height] = self.size.map(|side| side as f32);
        let area = [
            f32::from(self.cells[0]) * cell[0],
            f32::from(self.cells[1]) * cell[1],
        ];
        if width <= 0.0 || height <= 0.0 || cell[0] <= 0.0 || cell[1] <= 0.0 {
            return None;
        }
        let scale = (area[0] / width).min(area[1] / height);
        let drawn = [width * scale, height * scale];
        let origin = [(area[0] - drawn[0]) / 2.0, (area[1] - drawn[1]) / 2.0];
        let at = [f32::from(col) * cell[0], f32::from(row) * cell[1]];
        let left = at[0].max(origin[0]);
        let top = at[1].max(origin[1]);
        let right = (at[0] + cell[0]).min(origin[0] + drawn[0]);
        let bottom = (at[1] + cell[1]).min(origin[1] + drawn[1]);
        if right <= left || bottom <= top {
            return None;
        }
        Some(Slice {
            image: self.image,
            size: self.size,
            rect: [
                (left - at[0]) / cell[0],
                (top - at[1]) / cell[1],
                (right - left) / cell[0],
                (bottom - top) / cell[1],
            ],
            crop: [
                (left - origin[0]) / scale,
                (top - origin[1]) / scale,
                (right - left) / scale,
                (bottom - top) / scale,
            ],
            z: -1,
        })
    }
}

/// An image a terminal asks its owner to keep (see
/// [`Terminal::take_image_requests`](crate::Terminal::take_image_requests)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRequest {
    /// What the answer names it by.
    pub token: u64,
    /// The image's data.
    pub source: ImageSource,
    /// The most bytes the image may be, as kept.
    pub most: u64,
}

/// Where an image's data is, and how it's encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    /// kitty's: pixels or a PNG, compressed with zlib if `compressed`.
    Kitty {
        /// What the data holds, once read and uncompressed.
        pixels: Pixels,
        /// Whether it's compressed with zlib.
        compressed: bool,
        /// Where it is.
        data: KittyData,
    },
    /// A sixel image's data, after its `q` (see [`sixel::decode`]).
    Sixel(Vec<u8>),
    /// An image file's bytes as iTerm2's are sent, base64: PNG, JPEG, GIF
    /// or WebP.
    Encoded(Vec<u8>),
}

/// What kitty's data holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pixels {
    /// Three bytes a pixel, red, green and blue, row by row.
    Rgb {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// Four bytes a pixel, with alpha.
    Rgba {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// A PNG.
    Png,
}

/// Where kitty's data is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KittyData {
    /// Here: base64.
    Direct(Vec<u8>),
    /// In the file at `path`: `size` bytes from `offset`, or all of it
    /// from there if `size` is 0. If `temporary`, the file is to be taken
    /// away once read, if it's where kitty's rules allow that.
    File {
        /// The path, as the program named it: absolute, with no NUL.
        path: Vec<u8>,
        /// Where to start.
        offset: u64,
        /// How much to read.
        size: u64,
        /// Whether it's a temporary file.
        temporary: bool,
    },
    /// In the POSIX shared memory object `name`, which is to be taken
    /// away once read.
    Shared {
        /// Its name, with no `/` but perhaps a leading one, nor NUL.
        name: Vec<u8>,
        /// Where to start.
        offset: u64,
        /// How much to read.
        size: u64,
    },
}

/// An image kept, as the owner says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageKept {
    /// What it's known by.
    pub key: ImageKey,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
}

/// Why an image wasn't kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// Its file couldn't be read.
    Unreadable,
    /// It isn't an image that can be shown.
    NotAnImage,
    /// It's over the limit for images.
    TooBig,
    /// Too much is being done with images.
    Busy,
}

impl From<ImageError> for kitty::Error {
    fn from(error: ImageError) -> Self {
        match error {
            ImageError::Unreadable => Self::Unreadable,
            ImageError::NotAnImage => Self::BadData,
            ImageError::TooBig => Self::TooBig,
            ImageError::Busy => Self::Busy,
        }
    }
}

/// A terminal's images: what's being collected, what's kept, and what's
/// shown.
#[derive(Debug, Default)]
pub(crate) struct Graphics {
    /// The APC string being read.
    apc: Option<Apc>,
    /// kitty's image being sent in parts.
    upload: Option<Upload>,
    /// A sixel image's data being collected.
    sixel: Option<Collected>,
    /// kitty's images, by the program's IDs.
    images: HashMap<u32, Kept>,
    /// How many images have been transmitted, to tell the newest.
    transmitted: u64,
    /// The pictures shown, by number.
    pictures: HashMap<u32, Placed>,
    /// kitty's placements among them: image ID, placement ID and the
    /// picture's number, so a command naming some finds them at once.
    placements: BTreeSet<(u32, u32, u32)>,
    /// The table was found full, and a sweep didn't make room: until a
    /// picture goes, more are refused without sweeping again.
    full: bool,
    /// How far history had come when they were last forgotten.
    swept_end: u64,
    /// The most bytes of an image's data being sent may take now, if the
    /// owner says less than the limit (see `Terminal::set_upload_room`).
    room: Option<usize>,
    /// The number the next picture gets.
    next_picture: u32,
    /// Forget pictures no row shows once there are this many.
    sweep_at: usize,
    /// Virtual placements, by image ID and placement ID.
    virtuals: HashMap<(u32, u32), Virtual>,
    /// How many images, pictures and virtual placements hold each image.
    holders: HashMap<ImageKey, u32>,
    /// Images nothing holds now, for the owner to let go.
    released: Vec<ImageKey>,
    /// Images to be kept, for the owner to take.
    requests: Vec<ImageRequest>,
    /// The image the terminal waits for, and what to do once it's kept.
    waiting: Option<(u64, Then)>,
    /// The token of the next request.
    next_token: u64,
    /// The most bytes of images the terminal may hold: [`DEFAULT_LIMIT`]
    /// unless the owner says.
    limit: Option<u64>,
}

/// An APC string as it comes.
#[derive(Debug, Default)]
struct Apc {
    /// It's no kitty command, or its keys are too long.
    ignored: bool,
    /// Its first byte, `G`, has been seen.
    started: bool,
    /// Its keys, until the `;`.
    keys: Vec<u8>,
    /// What its payload is for, once its keys are read.
    role: Option<Role>,
}

/// What an APC string's payload is for.
#[derive(Debug)]
enum Role {
    /// Some of the image being sent in parts, or the first: `more` if
    /// another part follows.
    Upload { more: bool },
    /// A command of its own: its payload means nothing.
    Command(kitty::Control),
    /// Nothing: it's no command.
    Nothing,
}

/// kitty's image being sent in parts.
#[derive(Debug)]
struct Upload {
    /// The first part's keys.
    control: kitty::Control,
    /// The payload so far.
    data: Vec<u8>,
    /// More came than may be kept.
    over: bool,
}

/// A string's data collected, up to a limit.
#[derive(Debug, Default)]
struct Collected {
    data: Vec<u8>,
    over: bool,
}

/// An image kept for kitty's commands.
#[derive(Clone, Copy, Debug)]
struct Kept {
    key: ImageKey,
    size: [u32; 2],
    /// The number the program gave it, `I`, if any.
    number: u32,
    /// When it was transmitted, to tell the newest of a number.
    transmitted: u64,
}

/// A picture, and which of kitty's placements it is, if one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub(crate) struct Placed {
    picture: Picture,
    kitty: Option<(u32, u32)>,
}

/// What to do once an image is kept.
#[derive(Debug)]
enum Then {
    Kitty(kitty::Control),
    Sixel,
    Iterm(iterm::Args),
}

/// How a picture moves the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moves {
    /// To after its last column, on its last row, as kitty's and iTerm2's
    /// do.
    After,
    /// Not at all, nor does it scroll.
    Stays,
    /// To the row below it, where it started, as sixel's do.
    Below,
}

/// How a picture is to be laid out.
#[derive(Clone, Copy, Debug)]
struct Spec {
    /// The part of the image: left, top, width and height, a width or
    /// height of 0 for the rest.
    crop: [u32; 4],
    /// The columns and rows, 0 for as many as it takes.
    cells: [u32; 2],
    /// How far into its first cell it starts, in pixels.
    offset: [u32; 2],
    /// Given both columns and rows, whether to keep its proportions.
    contain: bool,
    z: i32,
    moves: Moves,
    kitty: Option<(u32, u32)>,
}

impl Spec {
    fn plain(moves: Moves) -> Self {
        Self {
            crop: [0; 4],
            cells: [0; 2],
            offset: [0; 2],
            contain: false,
            z: 0,
            moves,
            kitty: None,
        }
    }
}

impl Graphics {
    /// The most bytes of images the terminal may hold.
    fn limit(&self) -> u64 {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }

    /// The most bytes of base64 that encode `limit()` bytes, and no more
    /// than there's room for now.
    fn most_base64(&self) -> usize {
        let most = usize::try_from(self.limit().div_ceil(3) * 4).unwrap_or(usize::MAX);
        most.min(self.room.unwrap_or(usize::MAX))
    }

    /// Keeps `placed` as picture `number`, holding its image.
    fn insert_picture(&mut self, number: u32, placed: Placed) {
        if let Some((id, placement)) = placed.kitty {
            self.placements.insert((id, placement, number));
        }
        self.hold(placed.picture.image);
        if let Some(old) = self.pictures.insert(number, placed) {
            self.forget_placement(number, old);
        }
    }

    /// Forgets picture `number`, letting its image go.
    fn remove_picture(&mut self, number: u32) -> Option<Placed> {
        let placed = self.pictures.remove(&number)?;
        self.forget_placement(number, placed);
        Some(placed)
    }

    fn forget_placement(&mut self, number: u32, placed: Placed) {
        self.full = false;
        if let Some((id, placement)) = placed.kitty {
            self.placements.remove(&(id, placement, number));
        }
        self.unhold(placed.picture.image);
    }

    /// The pictures of kitty's images with IDs `ids`, of placement
    /// `placement`, or of any if it's 0.
    fn placed_of(&self, ids: std::ops::RangeInclusive<u32>, placement: u32) -> Vec<u32> {
        let (first, last) = (*ids.start(), *ids.end());
        if first > last {
            return Vec::new();
        }
        (self
            .placements
            .range((first, 0, 0)..=(last, u32::MAX, u32::MAX)))
        .filter(|&&(_, p, _)| placement == 0 || p == placement)
        .map(|&(.., number)| number)
        .collect()
    }

    /// Whether kitty's image `id` is shown by a picture or a virtual
    /// placement.
    fn shown(&self, id: u32) -> bool {
        !self.placed_of(id..=id, 0).is_empty()
            || self.virtuals.values().any(|virtual_| virtual_.id == id)
    }

    /// The bytes of images being sent held now.
    pub(crate) fn sending(&self) -> usize {
        self.upload.as_ref().map_or(0, |upload| upload.data.len())
            + self.sixel.as_ref().map_or(0, |sixel| sixel.data.len())
    }

    /// Whether an image is being sent in parts, or a sixel image's data
    /// collected.
    pub(crate) fn sending_parts(&self) -> bool {
        self.upload.is_some() || self.sixel.is_some()
    }

    /// Gives up on the image being sent, as one that never ends is.
    pub(crate) fn abandon(&mut self) {
        self.upload = None;
        if let Some(sixel) = &mut self.sixel {
            sixel.over = true;
            sixel.data = Vec::new();
        }
    }

    pub(crate) fn set_room(&mut self, room: usize) {
        self.room = Some(room);
    }

    pub(crate) fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    pub(crate) fn waiting_token(&self) -> Option<u64> {
        self.waiting.as_ref().map(|&(token, _)| token)
    }

    pub(crate) fn has_pictures(&self) -> bool {
        !self.pictures.is_empty()
    }

    pub(crate) fn has_images(&self) -> bool {
        !self.holders.is_empty()
    }

    pub(crate) fn set_limit(&mut self, limit: u64) {
        self.limit = Some(limit);
    }

    pub(crate) fn take_requests(&mut self) -> Vec<ImageRequest> {
        std::mem::take(&mut self.requests)
    }

    /// The images nothing holds now, but for any held again since.
    pub(crate) fn take_released(&mut self) -> Vec<ImageKey> {
        let mut released = std::mem::take(&mut self.released);
        released.retain(|key| !self.holders.contains_key(key));
        released.sort_unstable();
        released.dedup();
        released
    }

    pub(crate) fn picture(&self, number: u32) -> Option<&Picture> {
        self.pictures.get(&number).map(|placed| &placed.picture)
    }

    pub(crate) fn virtuals(&self) -> impl Iterator<Item = &Virtual> + '_ {
        self.virtuals.values()
    }

    /// One more thing holds `key`.
    fn hold(&mut self, key: ImageKey) {
        *self.holders.entry(key).or_default() += 1;
    }

    /// One less thing holds `key`: once nothing does, it's released.
    fn unhold(&mut self, key: ImageKey) {
        if let Some(count) = self.holders.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.holders.remove(&key);
                self.released.push(key);
            }
        }
    }

    /// Asks for `source` to be kept, and waits for it, to do `then`.
    fn request(&mut self, source: ImageSource, then: Then) {
        self.next_token += 1;
        let token = self.next_token;
        self.requests.push(ImageRequest {
            token,
            source,
            most: self.limit(),
        });
        self.waiting = Some((token, then));
    }

    /// Lets every image go, as a reset does.
    pub(crate) fn reset(&mut self) {
        let keys: Vec<ImageKey> = self.holders.drain().map(|(key, _)| key).collect();
        self.released.extend(keys);
        self.images.clear();
        self.pictures.clear();
        self.placements.clear();
        self.virtuals.clear();
        self.upload = None;
        self.sixel = None;
        self.apc = None;
    }

    // --- APC strings --------------------------------------------------------

    pub(crate) fn apc_start(&mut self) {
        self.apc = Some(Apc::default());
    }

    pub(crate) fn apc_put(&mut self, mut bytes: &[u8]) {
        let Some(mut apc) = self.apc.take() else {
            return;
        };
        if !apc.started && !apc.ignored {
            match bytes.split_first() {
                Some((b'G', rest)) => {
                    apc.started = true;
                    bytes = rest;
                }
                Some(_) => apc.ignored = true,
                None => {}
            }
        }
        if apc.ignored {
            self.apc = Some(apc);
            return;
        }
        if apc.role.is_none() {
            let end = bytes.iter().position(|&byte| byte == b';');
            let keys = &bytes[..end.unwrap_or(bytes.len())];
            if apc.keys.len() + keys.len() > kitty::LONGEST_KEYS {
                apc.ignored = true;
                self.apc = Some(apc);
                return;
            }
            apc.keys.extend_from_slice(keys);
            let Some(end) = end else {
                self.apc = Some(apc);
                return;
            };
            bytes = &bytes[end + 1..];
            apc.role = Some(self.role_of(&apc.keys));
        }
        if let Some(Role::Upload { .. }) = apc.role
            && let Some(upload) = &mut self.upload
        {
            let room = self.room.unwrap_or(usize::MAX) as u64;
            let most = match upload.control.medium {
                b'd' => (self.limit.unwrap_or(DEFAULT_LIMIT).div_ceil(3) * 4).min(room),
                _ => LONGEST_NAME as u64,
            };
            if (upload.data.len() + bytes.len()) as u64 > most {
                upload.over = true;
            } else if !upload.over {
                upload.data.extend_from_slice(bytes);
            }
        }
        self.apc = Some(apc);
    }

    /// What a kitty command with `keys` is for: a part of an image being
    /// sent, which a delete cuts short, or a command of its own.
    fn role_of(&mut self, keys: &[u8]) -> Role {
        let Some(control) = kitty::Control::parse(keys) else {
            return Role::Nothing;
        };
        // An answer echoed back; but a last part may say no keys at all.
        if control.answer_like && (!keys.is_empty() || self.upload.is_none()) {
            return Role::Nothing;
        }
        if let Some(upload) = &mut self.upload {
            if control.action == b'd' {
                self.upload = None;
                return Role::Command(control);
            }
            // Later parts say only `m`, and perhaps `q`.
            if control.quiet != 0 {
                upload.control.quiet = control.quiet;
            }
            return Role::Upload { more: control.more };
        }
        if matches!(control.action, b't' | b'T' | b'q') {
            let more = control.more;
            self.upload = Some(Upload {
                control,
                data: Vec::new(),
                over: false,
            });
            return Role::Upload { more };
        }
        Role::Command(control)
    }

    // --- Sixel ---------------------------------------------------------------

    pub(crate) fn sixel_start(&mut self) {
        self.sixel = Some(Collected::default());
    }

    pub(crate) fn sixel_put(&mut self, bytes: &[u8]) {
        let most = self.most_base64();
        if let Some(sixel) = &mut self.sixel {
            if sixel.data.len() + bytes.len() > most {
                sixel.over = true;
            } else if !sixel.over {
                sixel.data.extend_from_slice(bytes);
            }
        }
    }

    pub(crate) fn collecting_sixel(&self) -> bool {
        self.sixel.is_some()
    }
}

impl Emulator {
    /// The end of an APC string: a kitty command, or part of one.
    pub(crate) fn apc_end(&mut self, cancelled: bool) {
        let Some(mut apc) = self.graphics.apc.take() else {
            return;
        };
        if cancelled {
            // An image cut off part way isn't one.
            if let Some(Role::Upload { .. }) = apc.role {
                self.graphics.upload = None;
            }
            return;
        }
        if apc.ignored || !apc.started {
            return;
        }
        if apc.role.is_none() {
            let keys = std::mem::take(&mut apc.keys);
            apc.role = Some(self.graphics.role_of(&keys));
        }
        match apc.role {
            Some(Role::Upload { more: true } | Role::Nothing) | None => {}
            Some(Role::Upload { more: false }) => {
                if let Some(upload) = self.graphics.upload.take() {
                    self.kitty_transmit(upload);
                }
            }
            Some(Role::Command(control)) => self.kitty_command(&control),
        }
    }

    /// Answers kitty's command `control` for image `id`: `OK`, or the
    /// error, as `control` asks.
    fn kitty_answer(
        &mut self,
        control: &kitty::Control,
        id: u32,
        result: Result<(), kitty::Error>,
    ) {
        if !control.answers(result.is_ok()) {
            return;
        }
        let words = result.err().map_or("OK", kitty::Error::words);
        let answer = kitty::answer(control, id, words);
        self.reply(answer.as_bytes());
    }

    /// An image sent whole, to transmit, show, or ask about.
    fn kitty_transmit(&mut self, upload: Upload) {
        let Upload {
            control,
            data,
            over,
        } = upload;
        match self.kitty_source(&control, data, over) {
            Ok(source) => self.graphics.request(source, Then::Kitty(control)),
            Err(error) => self.kitty_answer(&control, control.id, Err(error)),
        }
    }

    /// Where the image `control` sends is, its payload `data`.
    fn kitty_source(
        &self,
        control: &kitty::Control,
        data: Vec<u8>,
        over: bool,
    ) -> Result<ImageSource, kitty::Error> {
        use kitty::Error;
        if control.id != 0 && control.number != 0 || control.parent {
            return Err(Error::Invalid);
        }
        if over {
            return Err(Error::TooBig);
        }
        let pixels = match control.format {
            24 | 32 => {
                let (width, height) = (control.width, control.height);
                if width == 0
                    || height == 0
                    || width > sixel::MOST_SIDE
                    || height > sixel::MOST_SIDE
                {
                    return Err(Error::Invalid);
                }
                if u64::from(width) * u64::from(height) * 4 > self.graphics.limit() {
                    return Err(Error::TooBig);
                }
                if control.format == 24 {
                    Pixels::Rgb { width, height }
                } else {
                    Pixels::Rgba { width, height }
                }
            }
            100 => Pixels::Png,
            _ => return Err(Error::Invalid),
        };
        let named = || -> Result<Vec<u8>, Error> {
            let name = BASE64.decode(&data).map_err(|_| Error::Unreadable)?;
            if name.is_empty() || name.contains(&0) {
                return Err(Error::Unreadable);
            }
            Ok(name)
        };
        let (offset, size) = (u64::from(control.offset), u64::from(control.size));
        let data = match control.medium {
            b'd' => KittyData::Direct(data),
            b'f' | b't' => {
                let path = named()?;
                if path.first() != Some(&b'/') {
                    return Err(Error::Unreadable);
                }
                KittyData::File {
                    path,
                    offset,
                    size,
                    temporary: control.medium == b't',
                }
            }
            b's' => {
                let name = named()?;
                let bare = name.strip_prefix(b"/").unwrap_or(&name);
                if bare.is_empty() || bare.contains(&b'/') || bare == b"." || bare == b".." {
                    return Err(Error::Unreadable);
                }
                KittyData::Shared { name, offset, size }
            }
            _ => return Err(Error::Invalid),
        };
        Ok(ImageSource::Kitty {
            pixels,
            compressed: control.compressed,
            data,
        })
    }

    /// A kitty command of its own: to show an image transmitted, delete,
    /// or animate, which isn't done.
    fn kitty_command(&mut self, control: &kitty::Control) {
        match control.action {
            b'p' => {
                let id = self.kitty_id(control);
                let result = match id {
                    Some(id) => self.kitty_place(control, id),
                    None => Err(kitty::Error::NoImage),
                };
                self.kitty_answer(control, id.unwrap_or(control.id), result);
            }
            b'd' => self.kitty_delete(control),
            _ => self.kitty_answer(control, control.id, Err(kitty::Error::Invalid)),
        }
    }

    /// The ID of the image `control` names: by its ID, or the newest of
    /// its number.
    fn kitty_id(&self, control: &kitty::Control) -> Option<u32> {
        if control.id != 0 {
            return self
                .graphics
                .images
                .contains_key(&control.id)
                .then_some(control.id);
        }
        if control.number == 0 {
            return None;
        }
        (self.graphics.images.iter())
            .filter(|(_, kept)| kept.number == control.number)
            .max_by_key(|(_, kept)| kept.transmitted)
            .map(|(&id, _)| id)
    }

    /// An ID no kept image has, for one the program only numbered: from
    /// the top down, as programs number theirs from the bottom up.
    fn free_id(&self) -> u32 {
        let mut id = u32::MAX;
        while self.graphics.images.contains_key(&id) {
            id -= 1;
        }
        id
    }

    /// What became of the image waited for, as the owner says: whether
    /// it was the one waited for. One that wasn't, as one given up on,
    /// the terminal holds nothing of.
    pub(crate) fn image_kept(&mut self, token: u64, result: Result<ImageKept, ImageError>) -> bool {
        let Some((waited, then)) = self.graphics.waiting.take() else {
            return false;
        };
        if waited != token {
            self.graphics.waiting = Some((waited, then));
            return false;
        }
        match then {
            Then::Kitty(control) => self.kitty_kept(&control, result),
            Then::Sixel => {
                if let Ok(kept) = result {
                    let _ = self.place_picture(kept, Spec::plain(Moves::Below));
                }
            }
            Then::Iterm(args) => {
                if let Ok(kept) = result {
                    let spec = self.iterm_spec(&args);
                    let _ = self.place_picture(kept, spec);
                }
            }
        }
        // What nothing holds is let go: an image only asked about, or
        // one whose picture couldn't be shown.
        if let Ok(kept) = result
            && !self.graphics.holders.contains_key(&kept.key)
        {
            self.graphics.released.push(kept.key);
        }
        true
    }

    /// kitty's image kept, or not: kept by its ID, and shown if asked.
    fn kitty_kept(&mut self, control: &kitty::Control, result: Result<ImageKept, ImageError>) {
        let kept = match result {
            Ok(kept) => kept,
            Err(error) => return self.kitty_answer(control, control.id, Err(error.into())),
        };
        if control.action == b'q' {
            return self.kitty_answer(control, control.id, Ok(()));
        }
        self.graphics.transmitted += 1;
        let new = Kept {
            key: kept.key,
            size: [kept.width, kept.height],
            number: control.number,
            transmitted: self.graphics.transmitted,
        };
        // An image with neither ID nor number can't be named again: it's
        // only shown.
        if control.id == 0 && control.number == 0 {
            if control.action == b'T' {
                let _ = self.kitty_show(control, 0, new);
            }
            return;
        }
        let id = if control.id != 0 {
            control.id
        } else {
            self.free_id()
        };
        self.graphics.hold(kept.key);
        if let Some(old) = self.graphics.images.insert(id, new) {
            self.graphics.unhold(old.key);
        }
        // Placeholders show what's transmitted by their image's ID now.
        let mut moved = Vec::new();
        for placement in self.graphics.virtuals.values_mut() {
            if placement.id == id && placement.image != kept.key {
                moved.push(placement.image);
                placement.image = kept.key;
                placement.size = new.size;
            }
        }
        for old in moved {
            self.graphics.hold(kept.key);
            self.graphics.unhold(old);
        }
        if self.graphics.images.len() > MOST_IMAGES
            && let Some((&oldest, _)) =
                (self.graphics.images.iter()).min_by_key(|(_, kept)| kept.transmitted)
        {
            let gone = self.graphics.images.remove(&oldest).expect("found above");
            self.graphics.unhold(gone.key);
        }
        let result = if control.action == b'T' {
            self.kitty_place(control, id)
        } else {
            Ok(())
        };
        // An image the program gave no ID or number is never answered.
        self.kitty_answer(control, id, result);
    }

    /// Shows kitty's image `id` as `control` says: as a picture at the
    /// cursor, or a virtual placement for Unicode placeholders.
    fn kitty_place(&mut self, control: &kitty::Control, id: u32) -> Result<(), kitty::Error> {
        let Some(&image) = self.graphics.images.get(&id) else {
            return Err(kitty::Error::NoImage);
        };
        self.kitty_show(control, id, image)
    }

    /// Shows `image`, kitty's image `id`, or 0 for one with none, as
    /// `control` says.
    fn kitty_show(
        &mut self,
        control: &kitty::Control,
        id: u32,
        image: Kept,
    ) -> Result<(), kitty::Error> {
        if control.parent || control.virtual_ && id == 0 {
            return Err(kitty::Error::Invalid);
        }
        if control.virtual_ {
            let cell = self.cell();
            let cells = [
                fit(control.cols, image.size[0], cell[0]),
                fit(control.rows, image.size[1], cell[1]),
            ];
            let key = (id, control.placement);
            if self.graphics.virtuals.len() >= MOST_VIRTUALS
                && !self.graphics.virtuals.contains_key(&key)
            {
                return Err(kitty::Error::Busy);
            }
            let placement = Virtual {
                id,
                placement: control.placement,
                image: image.key,
                size: image.size,
                cells,
            };
            self.graphics.hold(image.key);
            if let Some(old) = self.graphics.virtuals.insert(key, placement) {
                self.graphics.unhold(old.image);
            }
            return Ok(());
        }
        // A placement placed again moves.
        if control.placement != 0 && id != 0 {
            let again = self.graphics.placed_of(id..=id, control.placement);
            self.delete_numbers(again.into_iter().collect());
        }
        let spec = Spec {
            crop: [control.x, control.y, control.w, control.h],
            cells: [control.cols, control.rows],
            offset: [control.cell_x, control.cell_y],
            contain: false,
            z: control.z,
            moves: if control.stay {
                Moves::Stays
            } else {
                Moves::After
            },
            kitty: (id != 0).then_some((id, control.placement)),
        };
        let kept = ImageKept {
            key: image.key,
            width: image.size[0],
            height: image.size[1],
        };
        self.place_picture(kept, spec)
    }

    /// kitty's delete: the pictures, and perhaps the images, `control`
    /// names.
    fn kitty_delete(&mut self, control: &kitty::Control) {
        let free = control.delete.is_ascii_uppercase();
        let (x, y) = (control.x, control.y);
        let ids: Option<std::ops::RangeInclusive<u32>> = match control.delete.to_ascii_lowercase() {
            b'i' => Some(control.id..=control.id),
            b'n' => self.kitty_id(control).map(|id| id..=id),
            b'r' => Some(x..=y),
            _ => None,
        };
        let gone = if let Some(ids) = &ids {
            let placement = control.placement;
            self.graphics.virtuals.retain(|_, virtual_| {
                !(ids.contains(&virtual_.id) && (placement == 0 || virtual_.placement == placement))
            });
            let numbers = self.graphics.placed_of(ids.clone(), placement);
            self.delete_numbers(numbers.into_iter().collect())
        } else {
            let cursor = (self.cursor.col, self.cursor.line);
            let cell = (x.saturating_sub(1) as usize, y.saturating_sub(1) as usize);
            let z = control.z;
            let covers =
                |piece: Piece, picture: &Picture, (col, line): (usize, usize), at: usize| {
                    at == line
                        && (usize::from(piece.col)
                            ..usize::from(piece.col) + usize::from(picture.cells[0]))
                            .contains(&col)
                };
            match control.delete.to_ascii_lowercase() {
                b'a' => self.delete_pictures(|_, _| true),
                b'c' => self.delete_pictures(|placed, (piece, line)| {
                    covers(piece, &placed.picture, cursor, line)
                }),
                b'p' => self.delete_pictures(|placed, (piece, line)| {
                    covers(piece, &placed.picture, cell, line)
                }),
                b'q' => self.delete_pictures(|placed, (piece, line)| {
                    placed.picture.z == z && covers(piece, &placed.picture, cell, line)
                }),
                b'x' => self.delete_pictures(|placed, (piece, line)| {
                    covers(piece, &placed.picture, (cell.0, line), line)
                }),
                b'y' => self.delete_pictures(|_, (_, line)| line == cell.1),
                b'z' => self.delete_pictures(|placed, _| placed.picture.z == z),
                // Animation frames, of which there are none.
                _ => Vec::new(),
            }
        };
        if !free {
            return;
        }
        // Images go too, once nothing shows them.
        let mut ids: HashSet<u32> = gone
            .iter()
            .filter_map(|kitty| kitty.map(|(id, _)| id))
            .collect();
        if let Some(range) = &ids_range(control, x, y, &self.graphics) {
            ids.extend(range.iter().copied());
        }
        for id in ids {
            if !self.graphics.shown(id)
                && let Some(gone) = self.graphics.images.remove(&id)
            {
                self.graphics.unhold(gone.key);
            }
        }
    }

    /// Deletes the pictures `which` says to of those the screen shows,
    /// each given with one of its pieces and the screen line it's on (see
    /// [`delete_numbers`](Self::delete_numbers)). Says which of kitty's
    /// placements went.
    fn delete_pictures(
        &mut self,
        which: impl Fn(&Placed, (Piece, usize)) -> bool,
    ) -> Vec<Option<(u32, u32)>> {
        let mut gone = HashSet::new();
        for (line, row) in self.grid().rows().enumerate() {
            for &piece in row.pieces() {
                if let Some(placed) = self.graphics.pictures.get(&piece.picture)
                    && which(placed, (piece, line))
                {
                    gone.insert(piece.picture);
                }
            }
        }
        self.delete_numbers(gone)
    }

    /// Deletes the pictures numbered `gone`: their pieces go from the
    /// screens' rows at once, and from history's as it's next swept (see
    /// [`sweep_pictures`](Self::sweep_pictures)), as a picture no longer
    /// kept shows nothing meanwhile. So a delete touches only the screen,
    /// however much history there is. Says which of kitty's placements
    /// went.
    fn delete_numbers(&mut self, gone: HashSet<u32>) -> Vec<Option<(u32, u32)>> {
        if gone.is_empty() {
            return Vec::new();
        }
        let (primary, alternate) = self.grids_mut();
        primary.retain_pieces(false, |piece| !gone.contains(&piece.picture));
        if let Some(alternate) = alternate {
            alternate.retain_pieces(false, |piece| !gone.contains(&piece.picture));
        }
        (gone.into_iter())
            .filter_map(|number| Some(self.graphics.remove_picture(number)?.kitty))
            .collect()
    }

    /// Whether pictures no row shows are worth forgetting as output
    /// pauses: history has moved on far enough since they last were that
    /// the work of a sweep, a look at every piece history holds, is paid
    /// for by the lines it took. Those deleted, or on rows cleared, wait
    /// for it too, or for the table to fill.
    pub(crate) fn sweep_due(&self) -> bool {
        let primary = self.grids().0;
        let moved = primary
            .scrollback_end()
            .saturating_sub(self.graphics.swept_end);
        moved >= (primary.history_piece_count() as u64 / 8).max(256)
    }

    /// Forgets the pictures no row shows any more, and the pieces history
    /// holds of those deleted.
    pub(crate) fn sweep_pictures(&mut self) {
        let kept: HashSet<u32> = self.graphics.pictures.keys().copied().collect();
        self.grids_mut()
            .0
            .retain_pieces(true, |piece| kept.contains(&piece.picture));
        let (primary, alternate) = self.grids();
        let mut live: HashSet<u32> = primary.pieces().map(|(_, piece)| piece.picture).collect();
        if let Some(alternate) = alternate {
            live.extend(pieces_of(alternate).map(|piece| piece.picture));
        }
        let gone: Vec<u32> = (kept.into_iter())
            .filter(|number| !live.contains(number))
            .collect();
        for number in gone {
            self.graphics.remove_picture(number);
        }
        self.graphics.sweep_at = (self.graphics.pictures.len() * 2).max(64);
        self.graphics.swept_end = self.grids().0.scrollback_end();
    }

    /// How big a cell is, in pixels, as a window said, or as taken.
    fn cell(&self) -> [u32; 2] {
        self.cell_pixels().map_or(DEFAULT_CELL, |[width, height]| {
            [u32::from(width).max(1), u32::from(height).max(1)]
        })
    }

    /// Shows the image `kept` at the cursor, as `spec` says.
    #[allow(
        clippy::too_many_lines,
        reason = "one layout, worked out a step at a time"
    )]
    fn place_picture(&mut self, kept: ImageKept, spec: Spec) -> Result<(), kitty::Error> {
        let [cell_width, cell_height] = self.cell();
        let size = [kept.width, kept.height];
        let x = spec.crop[0].min(size[0]);
        let y = spec.crop[1].min(size[1]);
        let w = match spec.crop[2] {
            0 => size[0] - x,
            w => w.min(size[0] - x),
        };
        let h = match spec.crop[3] {
            0 => size[1] - y,
            h => h.min(size[1] - y),
        };
        if w == 0 || h == 0 {
            return Err(kitty::Error::Invalid);
        }
        let offset = [
            spec.offset[0].min(cell_width - 1),
            spec.offset[1].min(cell_height - 1),
        ];
        // How big it's drawn, in pixels of cells as they are now.
        let scaled = |given: u32, cell: u32, offset: u32| {
            u64::from(given.min(MOST_CELLS)) * u64::from(cell) - u64::from(offset)
        };
        let (drawn_w, drawn_h) = match spec.cells {
            [0, 0] => (u64::from(w), u64::from(h)),
            [cols, 0] => {
                let drawn_w = scaled(cols, cell_width, offset[0]);
                (drawn_w, u64::from(h) * drawn_w / u64::from(w))
            }
            [0, rows] => {
                let drawn_h = scaled(rows, cell_height, offset[1]);
                (u64::from(w) * drawn_h / u64::from(h), drawn_h)
            }
            [cols, rows] => {
                let (area_w, area_h) = (
                    scaled(cols, cell_width, offset[0]),
                    scaled(rows, cell_height, offset[1]),
                );
                if spec.contain {
                    // As big as fits, in proportion.
                    if area_w * u64::from(h) <= area_h * u64::from(w) {
                        (area_w, u64::from(h) * area_w / u64::from(w))
                    } else {
                        (u64::from(w) * area_h / u64::from(h), area_h)
                    }
                } else {
                    (area_w, area_h)
                }
            }
        };
        let (drawn_w, drawn_h) = (drawn_w.max(1), drawn_h.max(1));
        let cells_for = |drawn: u64, offset: u32, cell: u32, given: u32| {
            let needed = (drawn + u64::from(offset)).div_ceil(u64::from(cell));
            let cells = if given == 0 {
                needed
            } else {
                u64::from(given).max(needed)
            };
            u16::try_from(cells.min(u64::from(MOST_CELLS))).unwrap_or(u16::MAX)
        };
        let cols = cells_for(drawn_w, offset[0], cell_width, spec.cells[0]);
        let rows = cells_for(drawn_h, offset[1], cell_height, spec.cells[1]);
        let parts = |pixels: u64, cell: u32| {
            u32::try_from(pixels * u64::from(PARTS) / u64::from(cell)).unwrap_or(u32::MAX)
        };
        let picture = Picture {
            image: kept.key,
            size,
            crop: [x, y, w, h],
            cells: [cols, rows],
            offset: [
                u16::try_from(parts(u64::from(offset[0]), cell_width)).unwrap_or(0),
                u16::try_from(parts(u64::from(offset[1]), cell_height)).unwrap_or(0),
            ],
            span: [parts(drawn_w, cell_width), parts(drawn_h, cell_height)],
            z: spec.z,
        };
        let full = self.graphics.pictures.len() >= MOST_PICTURES;
        if self.graphics.pictures.len() >= self.graphics.sweep_at.max(64)
            || full && !self.graphics.full
        {
            self.sweep_pictures();
        }
        if self.graphics.pictures.len() >= MOST_PICTURES {
            // Not swept again until a picture goes.
            self.graphics.full = true;
            return Err(kitty::Error::Busy);
        }
        let number = loop {
            self.graphics.next_picture = self.graphics.next_picture.wrapping_add(1);
            let number = self.graphics.next_picture;
            if number != 0 && !self.graphics.pictures.contains_key(&number) {
                break number;
            }
        };
        self.graphics.insert_picture(
            number,
            Placed {
                picture,
                kitty: spec.kitty,
            },
        );
        self.lay_pieces(number, cols, rows, spec.moves);
        Ok(())
    }

    /// Puts the pieces of picture `number`, `cols` by `rows`, on the rows
    /// from the cursor's down, moving the cursor as `moves` says.
    fn lay_pieces(&mut self, number: u32, cols: u16, rows: u16, moves: Moves) {
        let start = self.cursor.col;
        let col = u16::try_from(start).unwrap_or(u16::MAX);
        let height = self.height();
        for row in 0..rows {
            let piece = Piece {
                picture: number,
                col,
                row,
            };
            if moves == Moves::Stays {
                let line = self.cursor.line + usize::from(row);
                if line >= height {
                    break;
                }
                self.grid_mut().row_mut(line).add_piece(piece);
                // A picture shown in a region is as private as text.
                if !self.regions.hinting.is_empty() {
                    self.hint_row(line);
                }
                continue;
            }
            if row > 0 {
                self.index();
            }
            let line = self.cursor.line;
            self.grid_mut().row_mut(line).add_piece(piece);
            if !self.regions.hinting.is_empty() {
                self.hint_row(line);
            }
        }
        match moves {
            Moves::Stays => {}
            Moves::After => {
                let width = self.width();
                let after = start + usize::from(cols);
                if after >= width {
                    self.cursor.col = width - 1;
                    self.cursor.pending_wrap = self.autowraps();
                } else {
                    self.cursor.col = after;
                    self.cursor.pending_wrap = false;
                }
            }
            Moves::Below => {
                self.index();
                self.cursor.col = start;
            }
        }
    }

    // --- Sixel and iTerm2 ----------------------------------------------------

    /// The end of a sixel image's data: kept, and shown.
    pub(crate) fn sixel_end(&mut self) {
        let Some(sixel) = self.graphics.sixel.take() else {
            return;
        };
        if sixel.over || sixel.data.is_empty() {
            return;
        }
        self.graphics
            .request(ImageSource::Sixel(sixel.data), Then::Sixel);
    }

    /// `OSC 1337`'s string after `1337;`, `string`: an inline image to
    /// keep and show, if it's one.
    pub(crate) fn iterm_image(&mut self, string: &[u8]) {
        let Some((args, data)) = iterm::parse(string) else {
            return;
        };
        let data = &string[data..];
        if data.is_empty() || data.len() > self.graphics.most_base64() {
            return;
        }
        self.graphics
            .request(ImageSource::Encoded(data.to_vec()), Then::Iterm(args));
    }

    /// How iTerm2 lays out an image, as `args` say.
    fn iterm_spec(&self, args: &iterm::Args) -> Spec {
        let [cell_width, cell_height] = self.cell();
        let side = |side: iterm::Side, cell: u32, of: usize| match side {
            iterm::Side::Auto => 0,
            iterm::Side::Cells(cells) => cells,
            iterm::Side::Pixels(pixels) => pixels.div_ceil(cell),
            iterm::Side::Percent(percent) => {
                u32::try_from(of)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(percent.min(100))
                    / 100
            }
        };
        let cells = [
            side(args.width, cell_width, self.width()),
            side(args.height, cell_height, self.height()),
        ];
        // A side left to the image, given the other, keeps its
        // proportions.
        Spec {
            cells,
            contain: !args.stretch,
            moves: if args.stay {
                Moves::Stays
            } else {
                Moves::After
            },
            ..Spec::plain(Moves::After)
        }
    }

    // --- What's shown ---------------------------------------------------------

    /// The size of the part of its image each picture shows, by number.
    pub(crate) fn picture_sizes(&self) -> HashMap<u32, [u32; 2]> {
        (self.graphics.pictures.iter())
            .map(|(&number, placed)| (number, [placed.picture.crop[2], placed.picture.crop[3]]))
            .collect()
    }

    /// Whether `image` is placed, but only on lines whose pictures may not
    /// be shown to a client that has lines with any of the hints `hidden`
    /// masked (see [`may_show`]): its pictures' pieces, in history and on
    /// screen, and the placeholders on screen of its virtual placements.
    pub(crate) fn image_hidden(&self, image: ImageKey, hidden: crate::Hints) -> bool {
        let of_image = |number: u32| {
            (self.graphics.picture(number)).is_some_and(|picture| picture.image == image)
        };
        let placed: Vec<u64> = (self.primary.pieces())
            .filter(|&(_, piece)| of_image(piece.picture))
            .map(|(line, _)| line)
            .chain(self.alternate_pieces(&of_image))
            .collect();
        let hints = |line: u64| {
            let end = self.primary.scrollback_end();
            match usize::try_from(line.wrapping_sub(end)) {
                Ok(row) if line >= end && row < self.grid().height() => {
                    self.grid().row(row).hints()
                }
                _ => (self.primary.history_hints(line..line + 1).next())
                    .map_or(crate::Hints::empty(), |(_, hints)| hints),
            }
        };
        let mut shown = placed.iter().any(|&line| may_show(hints(line), hidden));
        let mut anywhere = !placed.is_empty();
        let virtual_ = (self.graphics.virtuals.values()).any(|virtual_| virtual_.image == image);
        if virtual_ {
            for row in self.grid().rows().filter(|row| has_placeholders(row)) {
                for (_, said) in placeholders(row) {
                    let of_it = (self.graphics.virtuals.values()).any(|virtual_| {
                        virtual_.image == image
                            && virtual_.id == said.image
                            && (said.placement == 0 || virtual_.placement == said.placement)
                    });
                    if of_it {
                        anywhere = true;
                        shown |= may_show(row.hints(), hidden);
                    }
                }
            }
        }
        anywhere && !shown
    }

    /// The lines of the alternate screen, numbered on from the main
    /// screen's history, that hold a piece of a picture `of_image` says
    /// is the image's, while it shows.
    fn alternate_pieces<'a>(
        &'a self,
        of_image: &'a dyn Fn(u32) -> bool,
    ) -> impl Iterator<Item = u64> + 'a {
        let end = self.primary.scrollback_end();
        let alternate = (self.screen == crate::terminal::Screen::Alternate).then(|| self.grid());
        (alternate.into_iter())
            .flat_map(|grid| grid.rows().enumerate())
            .filter(move |(_, row)| row.pieces().iter().any(|piece| of_image(piece.picture)))
            .map(move |(row, _)| end + row as u64)
    }

    /// The images the screen shows, and those Unicode placeholders may.
    pub(crate) fn shown_images(&self) -> HashSet<ImageKey> {
        let mut shown: HashSet<ImageKey> = pieces_of(self.grid())
            .filter_map(|piece| self.graphics.picture(piece.picture))
            .map(|picture| picture.image)
            .collect();
        // A virtual placement's image, only while a placeholder of it is
        // on the screen.
        if !self.graphics.virtuals.is_empty() {
            for row in self.grid().rows().filter(|row| has_placeholders(row)) {
                for (_, said) in placeholders(row) {
                    let placed = (self.graphics.virtuals.values()).filter(|virtual_| {
                        virtual_.id == said.image
                            && (said.placement == 0 || virtual_.placement == said.placement)
                    });
                    shown.extend(placed.map(|virtual_| virtual_.image));
                }
            }
        }
        shown
    }
}

/// The IDs a delete by range or ID names, with an uppercase letter, to let
/// go of though nothing showed them.
fn ids_range(control: &kitty::Control, x: u32, y: u32, graphics: &Graphics) -> Option<Vec<u32>> {
    match control.delete {
        b'I' if control.placement == 0 => Some(vec![control.id]),
        b'R' => Some(
            (graphics.images.keys())
                .filter(|&&id| (x..=y).contains(&id))
                .copied()
                .collect(),
        ),
        _ => None,
    }
}

/// Whether the pictures on a line with `hints` may be shown to a client
/// that has lines with any of the hints `hidden` masked (see
/// [`crate::regions`]): not on a line masked, as hidden, or kept from
/// agents where they may see it. `Pictures::of_rows` and `of_history`
/// ask, and the daemon's `serve_image` (see
/// [`Terminal::image_hidden`](crate::Terminal::image_hidden)). A line
/// shown as it is, as the user asked, still shows no picture.
#[must_use]
pub fn may_show(hints: crate::Hints, hidden: crate::Hints) -> bool {
    !hints.intersects(hidden)
}

/// The pieces of pictures `grid`'s screen shows.
fn pieces_of(grid: &Grid) -> impl Iterator<Item = Piece> + '_ {
    grid.rows().flat_map(|row| row.pieces().iter().copied())
}

/// How many cells of `cell` pixels a side `pixels` long takes, unless
/// `given`.
fn fit(given: u32, pixels: u32, cell: u32) -> u16 {
    let cells = if given == 0 {
        pixels.div_ceil(cell.max(1))
    } else {
        given
    };
    u16::try_from(cells.clamp(1, MOST_CELLS)).unwrap_or(u16::MAX)
}

/// A terminal's images as plain data, to carry it into another process
/// with its [`Snapshot`](crate::Snapshot): what was shown and kept, but
/// not what was being sent or waited for, which is given up on. Frozen,
/// as the snapshot's other parts are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct GraphicsSnapshot {
    /// kitty's images: ID, image, size and number.
    images: Vec<(u32, ImageKey, [u32; 2], u32)>,
    /// The pictures, by number.
    pictures: Vec<(u32, Placed)>,
    /// The number the last picture got.
    next_picture: u32,
    /// The virtual placements.
    virtuals: Vec<Virtual>,
    /// The pieces the primary screen's lines show, numbered through its
    /// history and on through the screen.
    primary: Vec<(u64, Piece)>,
    /// And the alternate screen's, by its line.
    alternate: Vec<(u16, Piece)>,
}

impl GraphicsSnapshot {
    /// The images the terminal holds.
    #[must_use]
    pub fn images(&self) -> HashSet<ImageKey> {
        (self.images.iter().map(|&(_, key, ..)| key))
            .chain(self.pictures.iter().map(|(_, placed)| placed.picture.image))
            .chain(self.virtuals.iter().map(|virtual_| virtual_.image))
            .collect()
    }
}

impl Emulator {
    /// Its images as plain data: as a sweep would leave them, the pictures
    /// no row shows and the pieces of pictures deleted left out, so what's
    /// restored from it is the same again.
    pub(crate) fn graphics_snapshot(&self) -> GraphicsSnapshot {
        let mut images: Vec<_> = (self.graphics.images.iter())
            .map(|(&id, kept)| (id, kept.key, kept.size, kept.number))
            .collect();
        images.sort_unstable_by_key(|&(id, ..)| id);
        let mut virtuals: Vec<_> = self.graphics.virtuals.values().copied().collect();
        virtuals.sort_unstable_by_key(|virtual_| (virtual_.id, virtual_.placement));
        let kept = |piece: &Piece| self.graphics.pictures.contains_key(&piece.picture);
        let (primary, alternate) = self.grids();
        let primary: Vec<(u64, Piece)> = (primary.pieces())
            .filter(|(_, piece)| kept(piece))
            .collect();
        let alternate: Vec<(u16, Piece)> = alternate.map_or_else(Vec::new, |grid| {
            (grid.rows().enumerate())
                .flat_map(|(line, row)| {
                    let line = u16::try_from(line).unwrap_or(u16::MAX);
                    row.pieces().iter().map(move |&piece| (line, piece))
                })
                .filter(|(_, piece)| kept(piece))
                .collect()
        });
        let shown: HashSet<u32> = (primary.iter().map(|(_, piece)| piece.picture))
            .chain(alternate.iter().map(|(_, piece)| piece.picture))
            .collect();
        let mut pictures: Vec<_> = (self.graphics.pictures.iter())
            .filter(|(number, _)| shown.contains(number))
            .map(|(&number, &placed)| (number, placed))
            .collect();
        pictures.sort_unstable_by_key(|&(number, _)| number);
        GraphicsSnapshot {
            images,
            pictures,
            next_picture: self.graphics.next_picture,
            virtuals,
            primary,
            alternate,
        }
    }

    pub(crate) fn restore_graphics(&mut self, snapshot: &GraphicsSnapshot) {
        self.graphics.reset();
        self.graphics.released.clear();
        for (transmitted, &(id, key, size, number)) in (1..).zip(&snapshot.images) {
            self.graphics.images.insert(
                id,
                Kept {
                    key,
                    size,
                    number,
                    transmitted,
                },
            );
            self.graphics.hold(key);
        }
        self.graphics.transmitted = snapshot.images.len() as u64;
        for &(number, placed) in snapshot.pictures.iter().take(MOST_PICTURES) {
            self.graphics.insert_picture(number, placed);
        }
        self.graphics.next_picture = snapshot.next_picture;
        for &virtual_ in snapshot.virtuals.iter().take(MOST_VIRTUALS) {
            self.graphics
                .virtuals
                .insert((virtual_.id, virtual_.placement), virtual_);
            self.graphics.hold(virtual_.image);
        }
        let (primary, alternate) = self.grids_mut();
        primary.set_pieces(&snapshot.primary);
        if let Some(alternate) = alternate {
            let pieces: Vec<(u64, Piece)> = (snapshot.alternate.iter())
                .map(|&(line, piece)| (alternate.scrollback_end() + u64::from(line), piece))
                .collect();
            alternate.set_pieces(&pieces);
        }
        self.sweep_pictures();
    }
}
