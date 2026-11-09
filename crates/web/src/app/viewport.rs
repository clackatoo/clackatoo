//! Keyboard geometry can settle late: after Safari's focus and resize
//! events, or with no event of its own. Each transition reads it on every
//! frame for a moment and applies it when it moved; nothing runs on an
//! idle page.

use super::{App, Document, HtmlInputElement, HtmlTextAreaElement, Window, with_app};
use wasm_bindgen::{JsCast as _, prelude::Closure};

/// How long after a transition frames keep reading the geometry, in
/// milliseconds: past the latest late measurement seen (about a second).
const TRACK_FOR: f64 = 1_500.0;

#[derive(Default)]
pub(super) struct Tracking {
    frame_asked: bool,
    /// Until when frames keep reading, as `Date.now()` has it.
    until: f64,
    /// What the last frame applied, so a frame that reads the same does
    /// nothing.
    seen: Option<Geometry>,
    /// The next frame applies what it reads, moved or not: an event came.
    stale: bool,
    /// The numbers shown over the page, when asked for.
    pub(super) measures: Option<super::measures::Measures>,
}

/// What places the page: the visual viewport, and the layout viewport
/// under it.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Geometry {
    pub(super) top: f64,
    pub(super) height: f64,
    pub(super) scale: f64,
    pub(super) inner: f64,
    pub(super) client: i32,
}

impl Geometry {
    /// What it is now, if the browser has a visual viewport.
    pub(super) fn read(window: &Window, document: &Document) -> Option<Self> {
        let viewport = window.visual_viewport()?;
        Some(Self {
            top: viewport.offset_top(),
            height: viewport.height(),
            scale: viewport.scale(),
            inner: (window.inner_height().ok())
                .and_then(|height| height.as_f64())
                .unwrap_or_default(),
            client: document
                .document_element()
                .map_or(0, |root| root.client_height()),
        })
    }
}

pub(super) fn listen(window: &Window, document: &Document) {
    // The window's scroll too: Safari may pan the page to show a field the
    // keyboard would cover.
    for kind in ["resize", "orientationchange", "pageshow", "focus", "scroll"] {
        super::listen(window, kind, false, move |_| {
            with_app(|app| app.viewport_settling(kind));
        });
    }
    for kind in ["focusin", "focusout"] {
        super::listen(document, kind, false, move |_| {
            with_app(|app| app.viewport_settling(kind));
        });
    }
    // Dismissing the keyboard need not blur the textarea. Tapping it again
    // therefore need not generate another focus event.
    super::listen(document, "pointerup", false, |event| {
        if event.target().is_some_and(|target| {
            target.is_instance_of::<HtmlTextAreaElement>()
                || target.is_instance_of::<HtmlInputElement>()
        }) {
            with_app(|app| app.viewport_settling("tap"));
        }
    });
    if let Some(viewport) = window.visual_viewport() {
        for (kind, said) in [("resize", "vv resize"), ("scroll", "vv scroll")] {
            super::listen(&viewport, kind, false, move |_| {
                with_app(|app| app.viewport_settling(said));
            });
        }
    }
}

impl App {
    /// The keyboard, the screen or the page may be moving, as `why` says:
    /// the next frame applies the geometry, and frames after it apply it
    /// again whenever it moves, for `TRACK_FOR` from now. A late
    /// measurement, with no event of its own, is still caught; an idle
    /// page asks for no frames.
    pub(super) fn viewport_settling(&mut self, why: &str) {
        if self.page.is_none() && self.term_page.is_none() {
            return;
        }
        self.note_viewport(why);
        self.viewport.stale = true;
        self.viewport.until = js_sys::Date::now() + TRACK_FOR;
        self.viewport_frame_soon();
    }

    fn viewport_frame_soon(&mut self) {
        if self.viewport.frame_asked {
            return;
        }
        self.viewport.frame_asked = true;
        let frame = Closure::once_into_js(|| {
            with_app(App::viewport_frame);
        });
        let _ = self.window.request_animation_frame(frame.unchecked_ref());
    }

    fn viewport_frame(&mut self) {
        self.viewport.frame_asked = false;
        if self.page.is_none() && self.term_page.is_none() {
            return;
        }
        let read = Geometry::read(&self.window, &self.document);
        let moved = read != self.viewport.seen;
        if self.viewport.stale || moved {
            if moved && !self.viewport.stale {
                // Moved with no event saying so: what the measurements are for.
                self.note_viewport("frame");
            }
            self.viewport.stale = false;
            self.viewport.seen = read;
            self.viewport_changed();
        }
        self.show_measures();
        if js_sys::Date::now() < self.viewport.until {
            self.viewport_frame_soon();
        }
    }
}
