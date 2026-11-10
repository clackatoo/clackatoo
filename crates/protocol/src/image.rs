//! Images: the one layer every pane shows pictures through (CLACKA-239).
//!
//! The daemon keeps each image once, as it was sent, by an [`ImageId`]
//! made from its bytes, and whatever shows it, a chat's turn, a picture
//! pane or an editor's Markdown, says only which: clients fetch an image
//! once, with [`Request::Image`](crate::Request::Image), and keep it by its
//! ID. Each image belongs to something, an [`ImageFrom`], which a request
//! names: the daemon checks the image is that thing's, and a daemon
//! passing the request on finds by it the machine that has it.
//!
//! An image comes in parts of at most [`ImageAnswer::PART`] bytes, each
//! asked for in turn, as one may be larger than a frame. A window on the
//! same machine takes the image as it was sent; the web app, and a window
//! at the far end of a slow link, ask for a copy no bigger than it's shown
//! (`fit`), which the daemon makes on a thread of its own, saying when
//! it's ready with [`Event::Image`](crate::Event::Image).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{BufferId, TerminalId};

/// An image, by its bytes as the daemon keeps them: the first 16 bytes of
/// their SHA-256. The same image sent twice is kept once.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ImageId(pub [u8; 16]);

impl fmt::Display for ImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ImageId({self})")
    }
}

/// How an image's bytes are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageFormat {
    /// PNG, as pixels sent bare are kept too.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF: its first frame is shown.
    Gif,
    /// WebP.
    Webp,
}

impl ImageFormat {
    /// Its media type, as a `data:` URL names it.
    #[must_use]
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

/// What a client needs to know of an image before it has it: enough to
/// lay out the space it takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageInfo {
    /// Which image.
    pub id: ImageId,
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// How its bytes are encoded.
    pub format: ImageFormat,
    /// How many bytes it is, as kept.
    pub bytes: u64,
}

impl ImageInfo {
    /// The most pixels an image may have, whatever its bytes: what a
    /// client may allocate to decode one, at four bytes a pixel, 256 MiB.
    pub const MOST_PIXELS: u64 = 64 * 1024 * 1024;
    /// The longest side an image may have.
    pub const MOST_SIDE: u32 = 16384;

    /// Whether its size is one an image may have: neither side 0, nor
    /// longer than [`MOST_SIDE`](Self::MOST_SIDE), nor more pixels in
    /// all than [`MOST_PIXELS`](Self::MOST_PIXELS). Checked before
    /// anything is allocated for it.
    #[must_use]
    pub fn sized(width: u32, height: u32) -> bool {
        width > 0
            && height > 0
            && width <= Self::MOST_SIDE
            && height <= Self::MOST_SIDE
            && u64::from(width) * u64::from(height) <= Self::MOST_PIXELS
    }

    /// The most bytes decoding an image may take.
    pub const DECODING_BYTES: u64 = Self::MOST_PIXELS * 4 + 64 * 1024 * 1024;

    /// Whether it may be decoded within
    /// [`DECODING_BYTES`](Self::DECODING_BYTES): it's [`sized`](Self::sized)
    /// as an image may be; and a JPEG, whose decoder's buffers the decoding
    /// library's limit doesn't cover, takes no more at three bytes a pixel
    /// decoded and two for each of three components' coefficients, as a
    /// progressive one keeps.
    #[must_use]
    pub fn decodable(&self) -> bool {
        const JPEG_BYTES: u64 = 3 + 3 * 2;
        Self::sized(self.width, self.height)
            && (self.format != ImageFormat::Jpeg
                || u64::from(self.width) * u64::from(self.height) * JPEG_BYTES
                    <= Self::DECODING_BYTES)
    }
}

/// Identifies a picture pane: one image file shown. One pane shows each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PictureId(pub u32);

impl fmt::Display for PictureId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// What an image belongs to, which a request for it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageFrom {
    /// A terminal, or a chat, which is one: the images its turns show.
    Terminal(TerminalId),
    /// A picture pane: the image file it shows.
    Picture(PictureId),
    /// A buffer: the images its Markdown shows.
    Buffer(BufferId),
}

/// What a picture pane shows (see [`Request::AttachPicture`]).
///
/// [`Request::AttachPicture`]: crate::Request::AttachPicture
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picture {
    /// The file's path, as people write it, from `~` under the home
    /// folder, with anything that could go unseen shown.
    pub path: String,
    /// What's become of reading it.
    pub state: PictureState,
}

/// How a picture pane's file is going.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PictureState {
    /// It's being read.
    Loading,
    /// It's this image.
    Shown(ImageInfo),
    /// It couldn't be, and why: it isn't an image, say.
    Failed(String),
}

/// An answer to [`Request::Image`](crate::Request::Image) or
/// [`Request::ImageAt`](crate::Request::ImageAt).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageAnswer {
    /// Bytes of the image, or of the copy asked for: `bytes` from `at`,
    /// of `total` in all. What's sent describes itself by `info`, whose ID
    /// is the image's, whatever size the copy is.
    Part {
        /// The image sent: a copy's own size, format and length.
        info: ImageInfo,
        /// How many bytes it is in all.
        total: u64,
        /// Where these bytes start.
        at: u64,
        /// The bytes: at most [`ImageAnswer::PART`].
        bytes: Vec<u8>,
    },
    /// A copy at the size asked for is being made: an
    /// [`ImageNews::Scaled`] says when it's ready to be asked for again.
    Scaling,
    /// It was let go to keep the images within `limit` bytes: shown as a
    /// quiet word in its place.
    Dropped {
        /// The limit, in bytes.
        limit: u64,
    },
    /// There's no such image here, or not the asker's.
    Missing,
    /// The image at the path asked for, read and kept.
    Found(ImageInfo),
    /// The file at the path asked for is being read: an
    /// [`ImageNews::Found`] says what came of it.
    Loading,
    /// The file at the path asked for can't be shown, and why.
    Failed(String),
}

impl ImageAnswer {
    /// The most bytes a part carries.
    pub const PART: u64 = 1024 * 1024;
}

/// What became of image work a client asked for, which took a while
/// (see [`Event::Image`](crate::Event::Image)).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageNews {
    /// The copy of `image` at `fit` is ready to be asked for: or, if it
    /// couldn't be made, or kept, what's to be had instead, the image
    /// itself or a failure, which is all that's sent for it after.
    Scaled {
        /// Whose it is.
        from: ImageFrom,
        /// The image.
        image: ImageId,
        /// The size asked for.
        fit: [u32; 2],
    },
    /// The file at `path` from `buffer`'s folder was read, and what came
    /// of it: the image, or why not.
    Found {
        /// The buffer whose Markdown asked.
        buffer: BufferId,
        /// The path, as asked.
        path: String,
        /// The image, or why there isn't one.
        found: Result<ImageInfo, String>,
    },
}

/// `path`, a relative path as a Markdown file names an image, written
/// plainly: `.` parts left out, and each `..` taking off the part before
/// it. `None` if it's absolute, empty, or goes up out of where it starts.
#[must_use]
pub fn plain_path(path: &str) -> Option<String> {
    if path.starts_with('/') || path.contains('\0') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The words in place of an image let go to keep within `limit` bytes.
#[must_use]
pub fn dropped_words(limit: u64) -> String {
    format!(
        "[image dropped: over the {} MB limit]",
        limit / (1024 * 1024)
    )
}

/// The words in place of an image, in a pane's text read by an agent or
/// copied: its size, never its bytes.
#[must_use]
pub fn image_words(width: u32, height: u32) -> String {
    clackatoo_term::graphics::words(width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jpeg_too_big_to_decode_within_the_limit_isnt_decoded() {
        let info = |format, width, height| ImageInfo {
            id: ImageId([0; 16]),
            width,
            height,
            format,
            bytes: 1,
        };
        // 8192 × 8192 is 64 Mi pixels: a PNG is decoded, a JPEG isn't.
        assert!(info(ImageFormat::Png, 8192, 8192).decodable());
        assert!(!info(ImageFormat::Jpeg, 8192, 8192).decodable());
        assert!(info(ImageFormat::Jpeg, 6000, 4000).decodable());
        assert!(!info(ImageFormat::Png, 16385, 1).decodable());
    }

    #[test]
    fn paths_are_written_plainly_and_never_go_up_out_of_where_they_start() {
        assert_eq!(plain_path("img/plan.png").as_deref(), Some("img/plan.png"));
        assert_eq!(
            plain_path("./img//../img/plan.png").as_deref(),
            Some("img/plan.png")
        );
        assert_eq!(plain_path("a/../b.png").as_deref(), Some("b.png"));
        assert_eq!(plain_path("../b.png"), None);
        assert_eq!(plain_path("a/../../b.png"), None);
        assert_eq!(plain_path("/etc/passwd"), None);
        assert_eq!(plain_path("."), None);
    }
}
