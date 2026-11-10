//! The images the chat shown takes from the daemon's images: each `img`
//! its turns have, by its `data-image`, is shown once the state has it,
//! and until then watched, to be asked for once it's on the screen, at
//! the size it's shown (see `state::layer`). The browser decodes them.

use clackatoo_protocol::TerminalId;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlElement, IntersectionObserver, IntersectionObserverEntry};

use super::{App, with_app};
use crate::state::{Look, State, fit_of, image_id};

/// Watches a chat page's images, until they're on the screen. One for
/// each page: it stops as the page goes.
#[derive(Debug)]
pub(super) struct Watcher {
    observer: IntersectionObserver,
    /// What the observer calls, which lives as long as it does.
    _seen: Closure<dyn FnMut(js_sys::Array)>,
}

impl Watcher {
    /// A watcher, if the browser has what it takes.
    pub(super) fn new() -> Option<Self> {
        let seen = Closure::<dyn FnMut(js_sys::Array)>::new(|entries: js_sys::Array| {
            with_app(|app| app.images_seen(&entries));
        });
        let observer = IntersectionObserver::new(seen.as_ref().unchecked_ref()).ok()?;
        Some(Self {
            observer,
            _seen: seen,
        })
    }

    /// Shows the images under `within` that the state has, or knows won't
    /// come, and watches the rest.
    pub(super) fn show(&self, within: &Element, state: &mut State) {
        let Ok(images) = within.query_selector_all("img[data-image]:not([data-shown])") else {
            return;
        };
        for at in 0..images.length() {
            let Some(img) = images.item(at).and_then(|n| n.dyn_into::<Element>().ok()) else {
                continue;
            };
            let Some(image) = (img.get_attribute("data-image").as_deref()).and_then(image_id)
            else {
                continue;
            };
            if let Some(look) = state.image_look(image) {
                self.observer.unobserve(&img);
                shown(&img, look);
            } else if !img.has_attribute("data-watched") {
                let _ = img.set_attribute("data-watched", "");
                self.observer.observe(&img);
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // The observer calls nothing once its closure is gone.
        self.observer.disconnect();
    }
}

/// Shows `img` as `look` says.
fn shown(img: &Element, look: Look<'_>) {
    let _ = img.set_attribute("data-shown", "");
    match look {
        Look::Url(url) => {
            let _ = img.set_attribute("src", url);
        }
        Look::Dropped(words) => {
            let _ = img.set_attribute("alt", words);
            let _ = img.set_attribute("title", words);
            let _ = img.class_list().add_1("dropped");
            if let Some(img) = img.dyn_ref::<HtmlElement>() {
                let _ = img.style().remove_property("aspect-ratio");
            }
        }
        Look::Missing => {
            let _ = img.class_list().add_1("missing");
        }
    }
}

impl App {
    /// Shows, or watches, the chat page's images, if its turns were drawn
    /// or an image came: `fresh` if the page was made just now.
    pub(super) fn show_images(&mut self, fresh: bool) {
        let dirty = &self.state.dirty;
        let drawn = fresh
            || dirty.images
            || dirty.all_turns
            || dirty.prepended > 0
            || !dirty.turns.is_empty();
        if let Some(page) = self.page.as_ref().filter(|_| drawn)
            && let Some(watcher) = &page.images
        {
            watcher.show(&page.turns, &mut self.state);
        }
    }

    /// Images watched came on the screen, as `entries` say: each is asked
    /// for at its size there, in the screen's pixels, and watched no more.
    fn images_seen(&mut self, entries: &js_sys::Array) {
        let ratio = self.window.device_pixel_ratio();
        for entry in entries.iter() {
            let entry: IntersectionObserverEntry = entry.unchecked_into();
            if !entry.is_intersecting() {
                continue;
            }
            let img = entry.target();
            let rect = img.get_bounding_client_rect();
            // Folded away, it has no size: it's watched until it has.
            let Some(fit) = fit_of([rect.width(), rect.height()], ratio) else {
                continue;
            };
            let image = (img.get_attribute("data-image").as_deref()).and_then(image_id);
            let chat = (img.get_attribute("data-terminal")).and_then(|chat| chat.parse().ok());
            if let Some(watcher) = self.page.as_ref().and_then(|page| page.images.as_ref()) {
                watcher.observer.unobserve(&img);
            }
            if let Some(watcher) = (self.term_page.as_ref()).and_then(|page| page.images.as_ref()) {
                watcher.observer.unobserve(&img);
            }
            if let (Some(image), Some(chat)) = (image, chat) {
                self.state.want_image(TerminalId(chat), image, fit);
            }
        }
        self.send();
        self.images_again_later();
        self.draw_soon();
    }

    /// Asks again for images whose copies are being made once they've
    /// been waited for long enough, if any are: once, however many ask.
    pub(super) fn images_again_later(&mut self) {
        let Some(at) = self.state.images_again_at() else {
            return;
        };
        if at <= self.images_at {
            return;
        }
        self.images_at = at;
        let again = Closure::once_into_js(|| {
            with_app(|app| {
                app.state.images_again();
                app.send();
                app.images_again_later();
            });
        });
        #[allow(
            clippy::cast_possible_truncation,
            reason = "milliseconds, about a second"
        )]
        let wait = ((at - self.state.now).max(0.0) + 20.0) as i32;
        let _ = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(again.unchecked_ref(), wait);
    }
}
