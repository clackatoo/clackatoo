//! Keyboard geometry can settle after Safari's focus/resize events. Read it
//! on a frame and briefly recheck transitions; nothing polls an idle page.

use super::{App, Document, HtmlInputElement, HtmlTextAreaElement, Window, with_app};
use wasm_bindgen::{JsCast as _, prelude::Closure};

#[derive(Default)]
pub(super) struct Tracking {
    frame_asked: bool,
    transition: u32,
}

pub(super) fn listen(window: &Window, document: &Document) {
    for kind in ["resize", "orientationchange", "pageshow", "focus"] {
        super::listen(window, kind, false, |_| {
            with_app(App::viewport_settling);
        });
    }
    for kind in ["focusin", "focusout"] {
        super::listen(document, kind, false, |_| {
            with_app(App::viewport_settling);
        });
    }
    // Dismissing the keyboard need not blur the textarea. Tapping it again
    // therefore need not generate another focus event.
    super::listen(document, "pointerup", false, |event| {
        if event.target().is_some_and(|target| {
            target.is_instance_of::<HtmlTextAreaElement>()
                || target.is_instance_of::<HtmlInputElement>()
        }) {
            with_app(App::viewport_settling);
        }
    });
    if let Some(viewport) = window.visual_viewport() {
        for kind in ["resize", "scroll"] {
            super::listen(&viewport, kind, false, |_| {
                with_app(App::viewport_soon);
            });
        }
    }
}

impl App {
    fn viewport_soon(&mut self) {
        if self.viewport.frame_asked {
            return;
        }
        self.viewport.frame_asked = true;
        let update = Closure::once_into_js(|| {
            with_app(|app| {
                app.viewport.frame_asked = false;
                app.viewport_changed();
            });
        });
        let _ = self.window.request_animation_frame(update.unchecked_ref());
    }

    pub(super) fn viewport_settling(&mut self) {
        self.viewport_soon();
        self.viewport.transition = self.viewport.transition.wrapping_add(1);
        let transition = self.viewport.transition;
        // A bounded fallback for delayed or missing keyboard resize events,
        // including a restored home-screen app. Superseded checks do no work.
        for delay in [100, 350, 700, 1_100] {
            let check = Closure::once_into_js(move || {
                with_app(|app| {
                    if app.viewport.transition == transition {
                        app.viewport_soon();
                    }
                });
            });
            let _ = self
                .window
                .set_timeout_with_callback_and_timeout_and_arguments_0(
                    check.unchecked_ref(),
                    delay,
                );
        }
    }
}
