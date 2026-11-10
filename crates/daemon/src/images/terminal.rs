//! Terminals' images, as their programs send them (CLACKA-239, part 2):
//! kitty's pixels or PNGs, in the command or in a file, sixel images and
//! iTerm2's image files, made into what the store keeps, on its thread or
//! a reader of its own.
//!
//! A file kitty names is read wherever the terminal's daemon is, through
//! [`regular::open`], following a link at its path as kitty does, but
//! never under `/proc`, `/sys` or `/dev` (but for shared memory, under
//! `/dev/shm`). A temporary file is taken away once read only if its name
//! holds `tty-graphics-protocol` and it's in a folder for temporary files,
//! reached through no link, as kitty's rules say; a shared memory object
//! is too, as kitty takes it away. Either is read and taken away through
//! its folder, opened once, only if its name still names the file read,
//! and only if what was read looks like the image it's said to be. Every failure to read a
//! file is said alike (see [`UNREADABLE`]), so a program can't find out
//! what's there.

use std::ffi::OsStr;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path};

use base64::Engine as _;
use clackatoo_term::graphics::sixel;
use clackatoo_term::{ImageSource, KittyData, Pixels};

use super::{MIB, png_of};
use crate::regular::{self, Links};

/// What any failure to read a file kitty names is said as.
pub(crate) const UNREADABLE: &str = "its file couldn't be read";

/// What a terminal's program sent that couldn't be kept, as a kitty error
/// says it (see [`clackatoo_term::ImageError`]).
pub(crate) fn error_of(why: &str, file: bool) -> clackatoo_term::ImageError {
    use clackatoo_term::ImageError;
    if why.starts_with("too many") {
        ImageError::Busy
    } else if why.contains("limit for images") {
        ImageError::TooBig
    } else if file {
        ImageError::Unreadable
    } else {
        ImageError::NotAnImage
    }
}

/// Whether `source` is in a file, read by a reader of its own.
pub(crate) fn in_file(source: &ImageSource) -> bool {
    matches!(
        source,
        ImageSource::Kitty {
            data: KittyData::File { .. } | KittyData::Shared { .. },
            ..
        }
    )
}

/// About how many bytes `source` holds on its way to the thread.
pub(crate) fn weight(source: &ImageSource) -> u64 {
    let bytes = match source {
        ImageSource::Kitty {
            data: KittyData::Direct(data),
            ..
        }
        | ImageSource::Sixel(data)
        | ImageSource::Encoded(data) => data.len(),
        ImageSource::Kitty { .. } => 0,
    };
    bytes as u64
}

/// The image `source` holds, as the store keeps it: a PNG, JPEG, GIF or
/// WebP, of no more than `most` bytes.
pub(crate) fn bytes_of(source: ImageSource, most: u64) -> Result<Vec<u8>, String> {
    let too_big = || format!("it's over the {} MB limit for images", most / MIB);
    let not_an_image = || "it isn't an image that can be shown".to_owned();
    match source {
        ImageSource::Encoded(data) => {
            let bytes = base64(&data).ok_or_else(not_an_image)?;
            if bytes.len() as u64 > most {
                return Err(too_big());
            }
            Ok(bytes)
        }
        ImageSource::Sixel(data) => {
            let pixels = (most / 4).min(sixel::MOST_PIXELS);
            let image = sixel::decode(&data, pixels).map_err(|refused| match refused {
                sixel::Refused::TooBig => too_big(),
                sixel::Refused::Empty => not_an_image(),
            })?;
            png_of(image.width, image.height, &image.rgba)
        }
        ImageSource::Kitty {
            pixels,
            compressed,
            data,
        } => {
            // What the data is to be, once read and uncompressed.
            let size = match pixels {
                Pixels::Rgb { width, height } => u64::from(width) * u64::from(height) * 3,
                Pixels::Rgba { width, height } => u64::from(width) * u64::from(height) * 4,
                Pixels::Png => most,
            };
            if size > most {
                return Err(too_big());
            }
            let read_most = if compressed { most } else { size };
            let raw = match data {
                KittyData::Direct(data) => base64(&data).ok_or_else(not_an_image)?,
                KittyData::File {
                    path,
                    offset,
                    size,
                    temporary,
                } => {
                    let path = Path::new(OsStr::from_bytes(&path));
                    let span = (offset, size, read_most);
                    match temporary.then(|| temporary_place(path)).flatten() {
                        Some((dir, name)) => read_and_take_away(&dir, name, span, |bytes| {
                            looks_whole(pixels, compressed, read_most, bytes)
                        })?,
                        None => read_file(path, span)?,
                    }
                }
                KittyData::Shared { name, offset, size } => {
                    let name = name.strip_prefix(b"/").unwrap_or(&name);
                    let dir = regular::dir_below(Path::new("/dev/shm"), Path::new(""), false)
                        .map_err(|_| UNREADABLE.to_owned())?;
                    let span = (offset, size, read_most);
                    read_and_take_away(&dir, OsStr::from_bytes(name), span, |bytes| {
                        looks_whole(pixels, compressed, read_most, bytes)
                    })?
                }
            };
            let raw = if compressed {
                inflate(&raw, size).ok_or_else(not_an_image)?
            } else {
                raw
            };
            match pixels {
                Pixels::Png => Ok(raw),
                Pixels::Rgba { width, height } => {
                    let size = usize::try_from(size).unwrap_or(usize::MAX);
                    let raw = raw.get(..size).ok_or_else(not_an_image)?;
                    png_of(width, height, raw)
                }
                Pixels::Rgb { width, height } => {
                    if (raw.len() as u64) < size {
                        return Err(not_an_image());
                    }
                    let rgba: Vec<u8> = (raw.chunks_exact(3))
                        .take((size / 3) as usize)
                        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], u8::MAX])
                        .collect();
                    png_of(width, height, &rgba)
                }
            }
        }
    }
}

/// `data`, base64 with or without its padding.
fn base64(data: &[u8]) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT
        .decode(data)
        .ok()
}

/// `data` uncompressed with zlib, if it's no more than `most` bytes.
fn inflate(data: &[u8], most: u64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .take(most.saturating_add(1))
        .read_to_end(&mut out)
        .ok()?;
    (out.len() as u64 <= most).then_some(out)
}

/// Where to read a file, and how much: `size` bytes from `offset`, or all
/// of it from there if `size` is 0, and no more than `most`.
type Span = (u64, u64, u64);

/// The file at `path`, as much of it as `span` says, through a link at its
/// path as kitty reads one, but only a regular file, and not somewhere
/// programs mustn't have read for them. Every failure is said as
/// [`UNREADABLE`].
fn read_file(path: &Path, span: Span) -> Result<Vec<u8>, String> {
    let unreadable = || UNREADABLE.to_owned();
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(unreadable());
    }
    let refused = ["/proc", "/sys", "/dev"];
    if refused.iter().any(|dir| path.starts_with(dir)) {
        return Err(unreadable());
    }
    read_from(
        regular::open(path, Links::Follow).map_err(|_| unreadable())?,
        span,
    )
}

/// As much of `file` as `span` says.
fn read_from(mut file: std::fs::File, (offset, size, most): Span) -> Result<Vec<u8>, String> {
    let unreadable = || UNREADABLE.to_owned();
    let len = file.metadata().map_err(|_| unreadable())?.len();
    let wanted = if size == 0 {
        len.saturating_sub(offset)
    } else {
        size
    };
    if offset.saturating_add(wanted) > len || wanted > most {
        return Err(unreadable());
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| unreadable())?;
    let mut bytes = Vec::new();
    file.take(wanted)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() as u64 != wanted {
        return Err(unreadable());
    }
    Ok(bytes)
}

/// The file `name` in the folder `dir`, as much of it as `span` says, never
/// through a link, then taken away, if what was read `looks_whole` and the
/// name, looked at again just before, still names the file read. Both go
/// by the folder, so a path changed meanwhile can't lead elsewhere; a file
/// put in the name's place in the moment after that look could still be
/// taken away in its stead.
fn read_and_take_away(
    dir: &OwnedFd,
    name: &OsStr,
    span: Span,
    looks_whole: impl FnOnce(&[u8]) -> bool,
) -> Result<Vec<u8>, String> {
    let unreadable = || UNREADABLE.to_owned();
    let name = Path::new(name);
    if name.components().count() != 1 || name.components().any(|part| part == Component::ParentDir)
    {
        return Err(unreadable());
    }
    let file = regular::open_at(dir, name, Links::Refuse).map_err(|_| unreadable())?;
    let read = rustix::fs::fstat(&file).map_err(|_| unreadable())?;
    let bytes = read_from(file, span)?;
    if !looks_whole(&bytes) {
        return Ok(bytes);
    }
    // Taken away only if the name still names the file read, looked at
    // just before.
    let named = rustix::fs::statat(dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW);
    let same = named.is_ok_and(|named| (named.st_dev, named.st_ino) == (read.st_dev, read.st_ino));
    if same {
        let _ = rustix::fs::unlinkat(dir, name, rustix::fs::AtFlags::empty());
    }
    Ok(bytes)
}

/// Where kitty's rules let a temporary file at `path` be taken away once
/// read: its name holds `tty-graphics-protocol`, and it's in `/tmp`,
/// `/dev/shm` or the daemon's own folder for temporary files, reached
/// from there through no link. The folder, opened, and the file's name.
fn temporary_place(path: &Path) -> Option<(OwnedFd, &OsStr)> {
    let name = path.file_name()?;
    let marked = (name.as_bytes())
        .windows(b"tty-graphics-protocol".len())
        .any(|part| part == b"tty-graphics-protocol");
    if !marked {
        return None;
    }
    let parent = path.parent()?;
    let temporary = std::env::temp_dir();
    let roots = [
        Path::new("/tmp"),
        Path::new("/dev/shm"),
        temporary.as_path(),
    ];
    roots.iter().find_map(|root| {
        let below = parent.strip_prefix(root).ok()?;
        Some((regular::dir_below(root, below, false).ok()?, name))
    })
}

/// Whether `bytes`, read to be taken away, look like the image they're
/// said to be, as far as is cheap to tell: a PNG starts as one, zlib's
/// data as it does, and pixels are as many as their size says. One that
/// doesn't is left where it is.
fn looks_whole(pixels: Pixels, compressed: bool, size: u64, bytes: &[u8]) -> bool {
    if compressed {
        return bytes.first() == Some(&0x78);
    }
    match pixels {
        Pixels::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        Pixels::Rgb { .. } | Pixels::Rgba { .. } => bytes.len() as u64 >= size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kittys_pixels_are_kept_as_a_png() {
        let rgb = base64::engine::general_purpose::STANDARD.encode([255u8, 0, 0, 0, 255, 0]);
        let source = ImageSource::Kitty {
            pixels: Pixels::Rgb {
                width: 2,
                height: 1,
            },
            compressed: false,
            data: KittyData::Direct(rgb.into_bytes()),
        };
        let png = bytes_of(source, MIB).unwrap();
        assert_eq!(super::super::look_at(&png).unwrap().1, 2);
        // Too few pixels for the size said isn't an image.
        let source = ImageSource::Kitty {
            pixels: Pixels::Rgba {
                width: 2,
                height: 2,
            },
            compressed: false,
            data: KittyData::Direct(b"AAAA".to_vec()),
        };
        assert!(bytes_of(source, MIB).is_err());
    }

    #[test]
    fn a_sixel_image_is_no_bigger_than_the_default_limit_allows_whatever_the_limit() {
        // 5000 by 5000, said by its raster attributes: 25 million pixels.
        let source = ImageSource::Sixel(b"\"1;1;5000;5000~".to_vec());
        assert!(bytes_of(source, 1 << 40).unwrap_err().contains("limit"));
    }

    #[test]
    fn compressed_data_is_uncompressed_no_further_than_it_may_be() {
        use std::io::Write as _;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&[7u8; 4 * 64]).unwrap();
        let packed = encoder.finish().unwrap();
        assert_eq!(inflate(&packed, 256).map(|out| out.len()), Some(256));
        assert_eq!(inflate(&packed, 255), None);
    }

    #[test]
    fn files_are_read_only_where_they_may_be_and_said_alike_when_not() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("a");
        std::fs::write(&file, b"0123456789").unwrap();
        assert_eq!(read_file(&file, (2, 3, 100)).unwrap(), b"234");
        assert_eq!(read_file(&file, (4, 0, 100)).unwrap(), b"456789");
        for (path, offset, size, most) in [
            (file.as_path(), 8, 5, 100),
            (file.as_path(), 0, 0, 5),
            (Path::new("/proc/self/environ"), 0, 0, 100),
            (Path::new("/nowhere"), 0, 0, 100),
            (Path::new("relative"), 0, 0, 100),
            (folder.path(), 0, 0, 100),
        ] {
            assert_eq!(
                read_file(path, (offset, size, most)),
                Err(UNREADABLE.to_owned())
            );
        }
    }

    #[test]
    fn only_files_kitty_names_temporary_are_taken_away_and_never_through_a_link() {
        let temporary = std::env::temp_dir();
        let folder = tempfile::tempdir_in(&temporary).unwrap();
        let png = b"\x89PNG\r\n\x1a\nrest";
        let marked = folder.path().join("tty-graphics-protocol-1.png");
        std::fs::write(&marked, png).unwrap();
        let (dir, name) = temporary_place(&marked).expect("a temporary file kitty names");
        let read = read_and_take_away(&dir, name, (0, 0, 100), |bytes| {
            looks_whole(Pixels::Png, false, 100, bytes)
        });
        assert_eq!(read.unwrap(), png);
        assert!(!marked.exists());
        // Not marked in its own name, nor outside the temporary folders.
        let named_dir = folder.path().join("tty-graphics-protocol");
        std::fs::create_dir(&named_dir).unwrap();
        assert!(temporary_place(&named_dir.join("a.png")).is_none());
        assert!(temporary_place(Path::new("/home/tty-graphics-protocol.png")).is_none());
        // Nor through a link to another folder.
        let elsewhere = tempfile::tempdir_in(&temporary).unwrap();
        let link = folder.path().join("link");
        std::os::unix::fs::symlink(elsewhere.path(), &link).unwrap();
        let kept = elsewhere.path().join("tty-graphics-protocol-2.png");
        std::fs::write(&kept, png).unwrap();
        assert!(temporary_place(&link.join("tty-graphics-protocol-2.png")).is_none());
        assert!(kept.exists());
        // Nor another file put in its name's place after it was read.
        let swapped = folder.path().join("tty-graphics-protocol-4.png");
        std::fs::write(&swapped, png).unwrap();
        let (dir, name) = temporary_place(&swapped).unwrap();
        let _ = read_and_take_away(&dir, name, (0, 0, 100), |_| {
            let other = folder.path().join("other");
            std::fs::write(&other, b"someone else's").unwrap();
            std::fs::rename(&other, &swapped).unwrap();
            true
        });
        assert_eq!(std::fs::read(&swapped).unwrap(), b"someone else's");
        // Nor what doesn't look like the image it's said to be.
        let wrong = folder.path().join("tty-graphics-protocol-3.png");
        std::fs::write(&wrong, b"not a png").unwrap();
        let (dir, name) = temporary_place(&wrong).unwrap();
        let _ = read_and_take_away(&dir, name, (0, 0, 100), |bytes| {
            looks_whole(Pixels::Png, false, 100, bytes)
        });
        assert!(wrong.exists());
    }
}
