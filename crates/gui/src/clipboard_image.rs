//! Explicit chat pastes: read and encode on a worker, never during a frame.
use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clackatoo_protocol::ChatImage;

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
        data: STANDARD.encode(bytes),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
