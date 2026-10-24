//! Explicit chat pastes: read and encode on a worker, never during a frame.
use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clackatoo_protocol::{ChatImage, ChatPreview};

#[derive(Debug)]
pub(crate) enum Pasted {
    Image(ChatImage),
    Text(String),
}

pub(crate) fn read() -> anyhow::Result<Pasted> {
    let mut clipboard = arboard::Clipboard::new().context("no clipboard")?;
    match clipboard.get_image() {
        Ok(image) => encode(&image).map(Pasted::Image),
        Err(arboard::Error::ContentNotAvailable) => Ok(Pasted::Text(clipboard.get_text()?)),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn encode(image: &arboard::ImageData<'_>) -> anyhow::Result<ChatImage> {
    let width = u32::try_from(image.width)?;
    let height = u32::try_from(image.height)?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > ChatImage::MAX_PIXELS || image.bytes.len() as u64 != pixels * 4 {
        bail!("image must have at most 16 million pixels");
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&image.bytes)?;
    }
    if bytes.len().div_ceil(3) * 4 > ChatImage::MAX_DATA {
        bail!("image is too large (8 MiB encoded maximum)");
    }
    Ok(ChatImage {
        preview: Some(preview(image, width, height)),
        data: STANDARD.encode(bytes),
        width,
        height,
    })
}

/// Area-average on the clipboard worker: small text and thin screenshot
/// lines survive shrinking, and no decoding or resizing happens in a frame.
fn preview(image: &arboard::ImageData<'_>, width: u32, height: u32) -> ChatPreview {
    let largest = width.max(height).max(ChatPreview::SIDE);
    let w = u32::try_from(u64::from(width) * u64::from(ChatPreview::SIDE) / u64::from(largest))
        .unwrap_or(ChatPreview::SIDE)
        .max(1);
    let h = u32::try_from(u64::from(height) * u64::from(ChatPreview::SIDE) / u64::from(largest))
        .unwrap_or(ChatPreview::SIDE)
        .max(1);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let top = y * height / h;
        let bottom = ((y + 1) * height / h).max(top + 1);
        for x in 0..w {
            let left = x * width / w;
            let right = ((x + 1) * width / w).max(left + 1);
            let mut colour = [0_u64; 4];
            for row in top..bottom {
                for col in left..right {
                    let at = ((row * width + col) * 4) as usize;
                    let pixel = &image.bytes[at..at + 4];
                    for channel in 0..3 {
                        colour[channel] += u64::from(pixel[channel]) * u64::from(pixel[3]);
                    }
                    colour[3] += u64::from(pixel[3]);
                }
            }
            let count = u64::from((bottom - top) * (right - left));
            for channel in 0..3 {
                rgba.push(u8::try_from(colour[channel] / colour[3].max(1)).unwrap_or(255));
            }
            rgba.push(u8::try_from(colour[3] / count).unwrap_or(255));
        }
    }
    ChatPreview {
        original: 0,
        width: w,
        height: h,
        rgba: rgba.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_is_bounded_proportional_and_averages_with_alpha() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 0].repeat(512 * 256 / 2);
        let image = arboard::ImageData {
            width: 512,
            height: 256,
            bytes: pixels.into(),
        };
        let small = preview(&image, 512, 256);
        assert_eq!((small.width, small.height), (256, 128));
        assert!(small.valid());
        assert_eq!(&small.rgba[..4], &[255, 0, 0, 127]);
        let narrow = arboard::ImageData {
            width: 1,
            height: 1024,
            bytes: [5, 10, 15, 255].repeat(1024).into(),
        };
        let small = preview(&narrow, 1, 1024);
        assert_eq!((small.width, small.height), (1, 256));
        assert!(small.valid());
    }

    #[test]
    fn clipboard_pixels_become_a_png_without_changing_colours() {
        let rgba = vec![255, 0, 17, 255, 0, 55, 255, 128];
        let image = encode(&arboard::ImageData {
            width: 2,
            height: 1,
            bytes: rgba.clone().into(),
        })
        .unwrap();
        let bytes = STANDARD.decode(&image.data).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((info.width, info.height), (2, 1));
        assert_eq!(&decoded[..info.buffer_size()], &rgba);
    }
    #[test]
    fn enormous_empty_and_inconsistent_clipboard_images_are_refused() {
        for (width, height) in [(0, 1), (1, 1), (usize::MAX, 1), (8192, 8192)] {
            assert!(
                encode(&arboard::ImageData {
                    width,
                    height,
                    bytes: Vec::new().into()
                })
                .is_err()
            );
        }
    }
}
