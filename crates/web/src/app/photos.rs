//! User-selected photos are decoded locally, normalized to the existing PNG
//! message format, and submitted only when the user sends the draft.

use super::{App, with_app};
use crate::{action::Action, dom, state::State, ui::el};
use clackatoo_protocol::{ChatImage, ChatPreview, TerminalId};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    CanvasRenderingContext2d, Document, Element, File, HtmlCanvasElement, HtmlElement,
    HtmlInputElement, ImageBitmap,
};

#[derive(Debug)]
pub(super) struct Controls {
    pub input: HtmlInputElement,
    pub button: HtmlElement,
    pub previews: HtmlElement,
    shown: Vec<u64>,
    loading: bool,
}

impl Controls {
    pub(super) fn new(document: &Document) -> Self {
        let input = dom::element_of(
            document,
            &el("input")
                .attr("type", "file")
                .attr("id", "photo-input")
                .attr("accept", "image/*")
                .attr("multiple", "")
                .attr("hidden", "")
                .attr("aria-label", "Choose photos"),
        )
        .unchecked_into();
        let button = dom::element_of(
            document,
            &el("button")
                .class("attach-photos")
                .attr("aria-label", "Attach photos")
                .attr("title", "Attach photos")
                .action(&Action::AttachPhotos)
                .text("＋"),
        )
        .unchecked_into();
        let previews: HtmlElement = dom::element_of(
            document,
            &el("div")
                .class("draft-photos")
                .attr("aria-label", "Attached photos"),
        )
        .unchecked_into();
        previews.set_hidden(true);
        Self {
            input,
            button,
            previews,
            shown: Vec::new(),
            loading: false,
        }
    }

    pub(super) fn mount(&self, row: &Element, composer: &Element) {
        let _ = row.append_child(&self.button);
        let _ = row.append_child(&self.input);
        let _ = composer.append_child(&self.previews);
    }

    pub(super) fn draw(&mut self, document: &Document, state: &State, chat: TerminalId) {
        let _ = self.input.set_attribute("data-chat", &chat.0.to_string());
        let images = state.images.get(&chat).map_or(&[][..], Vec::as_slice);
        let ids: Vec<_> = images.iter().map(|a| a.id).collect();
        let loading = state.loading_images.contains(&chat);
        self.input
            .set_disabled(loading || state.submitting.contains(&chat));
        if self.input.disabled() || images.len() >= ChatImage::MAX_COUNT {
            let _ = self.button.set_attribute("disabled", "");
        } else {
            let _ = self.button.remove_attribute("disabled");
        }
        if self.shown == ids && self.loading == loading {
            return;
        }
        self.shown = ids;
        self.loading = loading;
        self.previews.set_hidden(images.is_empty() && !loading);
        self.previews.set_text_content(None);
        for (index, a) in images.iter().enumerate() {
            let mut tile = el("div").class("draft-photo");
            if let Some(preview) = &a.image.preview {
                tile = tile.child(
                    el("canvas")
                        .preview(preview)
                        .attr("role", "img")
                        .attr("aria-label", a.image.label()),
                );
            }
            tile = tile.child(
                el("button")
                    .action(&Action::RemovePhoto(a.id))
                    .attr("aria-label", format!("Remove photo {}", index + 1))
                    .text("×"),
            );
            let _ = self
                .previews
                .append_child(&dom::element_of(document, &tile));
        }
        if loading {
            let _ = self.previews.append_child(&dom::element_of(
                document,
                &el("span")
                    .class("muted")
                    .attr("role", "status")
                    .text("Preparing photos…"),
            ));
        }
    }
}

pub(super) fn listen(root: &Element) {
    super::listen(root, "change", false, |event| {
        let Some(input) = event
            .target()
            .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            .filter(|i| i.id() == "photo-input")
        else {
            return;
        };
        let Some(list) = input.files() else {
            return;
        };
        let files: Vec<_> = (0..list.length().min(5))
            .filter_map(|i| list.item(i))
            .collect();
        input.set_value(""); // Choosing the same photo again must fire change.
        if !files.is_empty()
            && let Some(chat) = input
                .get_attribute("data-chat")
                .and_then(|id| id.parse().ok())
        {
            with_app(|app| app.prepare_photos(TerminalId(chat), files));
        }
    });
}

impl App {
    pub(super) fn attach_photos(&mut self) {
        // Keep the user's activation, without synthesizing another click into
        // the same delegated listener (a reentrant wasm closure).
        if let Some(page) = &self.page
            && page.photos.input.show_picker().is_err()
        {
            self.state.note =
                Some("The photo picker could not open. Try tapping Attach photos again.".into());
            self.state.dirty.footer = true;
        }
    }

    pub(super) fn remove_photo(&mut self, id: u64) {
        if let Some(open) = &self.state.open {
            self.state.remove_image(open.id, id);
        }
    }

    fn prepare_photos(&mut self, chat: TerminalId, files: Vec<File>) {
        if self.state.loading_images.contains(&chat) || self.state.submitting.contains(&chat) {
            return;
        }
        let count = self.state.images.get(&chat).map_or(0, Vec::len);
        if count + files.len() > ChatImage::MAX_COUNT {
            self.state.note = Some("Attach up to four photos per message.".into());
            self.state.dirty.footer = true;
            self.draw_soon();
            return;
        }
        self.state.loading_images.insert(chat);
        self.state.note = None;
        self.state.dirty.footer = true;
        self.draw_soon();
        spawn_local(async move {
            let mut errors = Vec::new();
            for file in files {
                let result = decode(file).await;
                with_app(|app| {
                    if let Err(error) = result.and_then(|image| app.state.add_image(chat, image)) {
                        errors.push(error);
                    }
                    app.draw_soon();
                });
            }
            with_app(|app| {
                app.state.loading_images.remove(&chat);
                if !errors.is_empty() {
                    app.state.note = Some(errors.join(" "));
                }
                app.state.dirty.footer = true;
                app.draw_soon();
            });
        });
    }
}

async fn decode(file: File) -> Result<ChatImage, String> {
    if file.size() > 20.0 * 1024.0 * 1024.0 {
        return Err("Choose a photo smaller than 20 MiB.".into());
    }
    if !file.type_().starts_with("image/") || file.type_() == "image/svg+xml" {
        return Err("Choose a photo or screenshot, such as PNG or JPEG.".into());
    }
    let window = web_sys::window().ok_or("The browser window is unavailable.")?;
    let promise = window
        .create_image_bitmap_with_blob(&file)
        .map_err(|_| "This browser could not open the photo.")?;
    let bitmap: ImageBitmap = JsFuture::from(promise)
        .await
        .map_err(|_| "This photo could not be decoded. Try a PNG or JPEG export.")?
        .dyn_into()
        .map_err(|_| "This photo could not be decoded.")?;
    let result = encode(&bitmap)
        .map_err(|_| "This photo could not be prepared. Try a smaller PNG or JPEG.".to_owned());
    bitmap.close();
    result
}

fn canvas(
    bitmap: &ImageBitmap,
    side: u32,
) -> Result<(HtmlCanvasElement, CanvasRenderingContext2d), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or(JsValue::NULL)?;
    let canvas: HtmlCanvasElement = document.create_element("canvas")?.dyn_into()?;
    let longest = bitmap.width().max(bitmap.height()).max(1);
    let width =
        (u64::from(bitmap.width()) * u64::from(side.min(longest)) / u64::from(longest)).max(1);
    let height =
        (u64::from(bitmap.height()) * u64::from(side.min(longest)) / u64::from(longest)).max(1);
    canvas.set_width(u32::try_from(width).map_err(|_| JsValue::NULL)?);
    canvas.set_height(u32::try_from(height).map_err(|_| JsValue::NULL)?);
    let context: CanvasRenderingContext2d =
        canvas.get_context("2d")?.ok_or(JsValue::NULL)?.dyn_into()?;
    context.draw_image_with_image_bitmap_and_dw_and_dh(
        bitmap,
        0.0,
        0.0,
        f64::from(canvas.width()),
        f64::from(canvas.height()),
    )?;
    Ok((canvas, context))
}

fn encode(bitmap: &ImageBitmap) -> Result<ChatImage, JsValue> {
    let (full, _) = canvas(bitmap, 4096)?;
    let url = full.to_data_url_with_type("image/png")?;
    let data = url
        .strip_prefix("data:image/png;base64,")
        .ok_or(JsValue::NULL)?;
    if data.len() > ChatImage::MAX_DATA {
        return Err(JsValue::NULL);
    }
    let (small, context) = canvas(bitmap, ChatPreview::SIDE)?;
    let rgba = context
        .get_image_data(
            0.0,
            0.0,
            f64::from(small.width()),
            f64::from(small.height()),
        )?
        .data()
        .0;
    let image = ChatImage {
        data: data.to_owned(),
        width: full.width(),
        height: full.height(),
        preview: Some(ChatPreview {
            width: small.width(),
            height: small.height(),
            rgba: rgba.into(),
            original: 0,
        }),
    };
    full.set_width(0);
    small.set_width(0);
    Ok(image)
}
