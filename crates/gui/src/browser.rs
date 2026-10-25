//! A browser pane: the page a chat's agent drives in clackatoo's browser
//! (see `clackatoo_protocol`'s browser module), as the daemon sends it.
//!
//! Its first line is the page's address and title, its second what the
//! agent did last, or why there's no page; below them, the latest picture
//! of the page, as big as fits, in proportion. Pictures are decoded on the
//! session's thread as they come ([`decode`]), never while drawing.
//!
//! | Key | Does |
//! |---|---|
//! | `q` | Closes the pane: the page stays the chat's |

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clackatoo_protocol::{Attributes, BrowserStatus, Color, Size, Style, TerminalId};
use clackatoo_term::{Row, RowBuilder};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::colors;
use crate::render::Image;
use crate::vim::Stroke;

/// How many lines the pane's words take, above the picture.
pub(crate) const STATUS_ROWS: u16 = 2;

/// The most a picture may be, in pixels each way: bigger isn't decoded,
/// so continuously streamed browser frames stay bounded independently of
/// on-demand original images.
const MAX_SIDE: usize = 4096;

/// What the pane asks of the desk, which it can't do itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Close the pane.
    Close,
}

/// A pane showing a chat's page.
#[derive(Debug)]
pub(crate) struct BrowserPane {
    chat: TerminalId,
    status: BrowserStatus,
    /// The latest picture of the page.
    image: Option<Arc<Image>>,
    title: String,
    width: usize,
    /// The words' rows, as of the last settle.
    rows: Vec<Row>,
    stale: bool,
    asks: Vec<Ask>,
}

impl BrowserPane {
    pub(crate) fn new(chat: TerminalId) -> Self {
        Self {
            chat,
            status: BrowserStatus::default(),
            image: None,
            title: format!("chat {chat}'s browser"),
            width: 0,
            rows: Vec::new(),
            stale: true,
            asks: Vec::new(),
        }
    }

    /// The chat whose page it shows.
    pub(crate) fn chat(&self) -> TerminalId {
        self.chat
    }

    /// What the window's title says: the page's title, or which chat's
    /// browser it is.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// What the bar says.
    pub(crate) fn label(&self) -> String {
        format!("{}  ·  q closes", self.title)
    }

    pub(crate) fn resize(&mut self, size: Size) {
        let width = usize::from(size.cols());
        self.stale |= width != self.width;
        self.width = width;
    }

    /// The page is now as `status` says.
    pub(crate) fn set_status(&mut self, status: BrowserStatus) {
        if status.running && !status.title.is_empty() {
            status.title.clone_into(&mut self.title);
        } else if !status.running {
            self.title = format!("chat {}'s browser", self.chat);
            self.image = None;
        }
        self.status = status;
        self.stale = true;
    }

    /// The latest picture of the page is `image`.
    pub(crate) fn set_image(&mut self, image: Arc<Image>) {
        self.image = Some(image);
    }

    /// The latest picture of the page, if there is one.
    pub(crate) fn image(&self) -> Option<&Image> {
        self.image.as_deref()
    }

    /// Lays out the words, if they changed.
    pub(crate) fn settle(&mut self) {
        if !std::mem::take(&mut self.stale) {
            return;
        }
        let faint = colors::line_number(false);
        let bold = Style {
            attributes: Attributes::BOLD,
            ..Style::DEFAULT
        };
        let mut first = RowBuilder::new(self.width);
        let mut second = RowBuilder::new(self.width);
        match &self.status {
            status if status.running => {
                first.push(&status.url, &Style::DEFAULT, true);
                if !status.title.is_empty() {
                    first.push(&format!("  {}", status.title), &faint, true);
                }
                let did = status.action.as_deref().unwrap_or("Nothing done yet");
                second.push(did, &faint, true);
            }
            status => {
                first.push(&self.title, &bold, true);
                let why = match &status.failure {
                    Some(why) => why.clone(),
                    None => "No page is open: the agent opens one with browser_navigate, once \
                             the chat's Browser setting is on"
                        .to_owned(),
                };
                let style = if status.failure.is_some() {
                    Style {
                        fg: Color::Indexed(1),
                        ..Style::DEFAULT
                    }
                } else {
                    faint
                };
                second.push(&why, &style, true);
            }
        }
        self.rows = vec![first.finish(false), second.finish(false)];
    }

    /// The rows to draw: the words. The picture is placed under them.
    pub(crate) fn scene(&self) -> Vec<&Row> {
        self.rows.iter().collect()
    }

    // Takes `stroke`. Returns whether the pane must be drawn again.
    crate::shortcuts::bindings::described_keys! {
        pub(crate) fn stroke / describe_stroke(&mut self, stroke: Stroke) -> bool;
        match stroke => {
            Stroke::Char {
                ch: 'q',
                ctrl: false,
            } =>
                crate::shortcuts::bindings::Hint::Action("Close pane, keeping browser session"),
                {
                    self.asks.push(Ask::Close);
                    true
                };
            _ =>
                crate::shortcuts::bindings::Hint::None,
                false;
        }
        result
    }

    pub(crate) fn shortcut_context(&self) -> crate::shortcuts::Context {
        crate::shortcuts::bindings::list("Browser pane", None, |stroke, _| {
            self.describe_stroke(stroke)
        })
    }

    /// What the pane asks of the desk, oldest first.
    pub(crate) fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asks)
    }
}

/// Where a picture `size` pixels goes in `area` (left, top, width and
/// height, in pixels): as big as fits, in proportion, centred across and
/// at the top. `None` if there's no room, or no picture.
pub(crate) fn fit(size: [u32; 2], area: [f32; 4]) -> Option<[f32; 4]> {
    let [left, top, width, height] = area;
    #[allow(clippy::cast_precision_loss)]
    let (w, h) = (size[0] as f32, size[1] as f32);
    if w < 1.0 || h < 1.0 || width < 1.0 || height < 1.0 {
        return None;
    }
    let scale = (width / w).min(height / h);
    let (drawn_w, drawn_h) = ((w * scale).floor(), (h * scale).floor());
    Some([
        (left + (width - drawn_w) / 2.0).floor(),
        top,
        drawn_w,
        drawn_h,
    ])
}

/// The JPEG `jpeg`, decoded, numbered as no picture before it. `None` if
/// it isn't one, or is too big.
pub(crate) fn decode(jpeg: &[u8]) -> Option<Image> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    let rgba = decoder
        .decode()
        .map_err(|err| log::debug!("a picture of a page couldn't be decoded: {err:?}"))
        .ok()?;
    let (width, height) = decoder.dimensions()?;
    Some(Image {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fits_in_proportion_at_the_top() {
        // Twice as wide as tall, in a square: as wide as it, half as tall.
        assert_eq!(
            fit([200, 100], [10.0, 20.0, 100.0, 100.0]),
            Some([10.0, 20.0, 100.0, 50.0])
        );
        // Taller than the room: as tall as it, centred across.
        assert_eq!(
            fit([100, 100], [0.0, 0.0, 300.0, 100.0]),
            Some([100.0, 0.0, 100.0, 100.0])
        );
        assert_eq!(fit([0, 100], [0.0, 0.0, 10.0, 10.0]), None);
        assert_eq!(fit([10, 10], [0.0, 0.0, 0.0, 10.0]), None);
    }

    #[test]
    fn a_picture_chromium_took_is_decoded_to_its_pixels() {
        // Red on its left half, blue on its right, 16 by 8.
        let first = decode(include_bytes!("../tests/fixtures/red-blue.jpg")).unwrap();
        let again = decode(include_bytes!("../tests/fixtures/red-blue.jpg")).unwrap();
        assert_eq!((first.width, first.height), (16, 8));
        assert_eq!(first.rgba.len(), 16 * 8 * 4);
        assert_ne!(first.id, again.id, "each picture is numbered afresh");
        let pixel = |x: usize| &first.rgba[(4 * 16 + x) * 4..(4 * 16 + x) * 4 + 4];
        let (left, right) = (pixel(2), pixel(13));
        assert!(left[0] > 200 && left[2] < 60, "{left:?}");
        assert!(right[2] > 200 && right[0] < 60, "{right:?}");
        assert_eq!(left[3], 255);
    }

    #[test]
    fn a_picture_wider_than_the_window_draws_isnt_decoded() {
        let mut jpeg = include_bytes!("../tests/fixtures/red-blue.jpg").to_vec();
        // The frame's header: its height, then its width, after the
        // marker, the header's length and the sample precision.
        let at = (jpeg.windows(2))
            .position(|pair| pair == [0xFF, 0xC0])
            .expect("a baseline JPEG");
        jpeg[at + 7..at + 9].copy_from_slice(&4097_u16.to_be_bytes());
        assert_eq!(decode(&jpeg), None);
    }

    #[test]
    fn what_isnt_a_jpeg_isnt_decoded() {
        assert_eq!(decode(b"not a picture"), None);
        assert_eq!(decode(&[]), None);
    }

    #[test]
    fn the_words_say_what_the_page_is_and_what_was_done() {
        let mut pane = BrowserPane::new(TerminalId(4));
        pane.resize(Size::new(60, 10).unwrap());
        pane.settle();
        let text = |pane: &BrowserPane| -> Vec<String> {
            pane.scene()
                .iter()
                .map(|row| row.text().trim_end().to_owned())
                .collect()
        };
        assert_eq!(text(&pane)[0], "chat 4's browser");
        assert!(text(&pane)[1].starts_with("No page is open"));
        pane.set_status(BrowserStatus {
            url: "https://example.com/".into(),
            title: "Example".into(),
            action: Some("Clicked link \"More\"".into()),
            running: true,
            failure: None,
        });
        pane.settle();
        assert_eq!(
            text(&pane),
            ["https://example.com/  Example", "Clicked link \"More\""]
        );
        assert_eq!(pane.title(), "Example");
        assert!(pane.stroke(Stroke::Char {
            ch: 'q',
            ctrl: false
        }));
        assert_eq!(pane.take_asks(), [Ask::Close]);
        assert!(!pane.stroke(Stroke::Char {
            ch: 'j',
            ctrl: false
        }));
    }
}
