//! The numbers that place the page, shown over it when asked for at the
//! foot of the list (Screen measurements): what the phone says of its
//! viewports as the keyboard comes and goes, and when, for a screenshot
//! from the phone itself. The page is placed by the browser's numbers
//! alone, and a phone's browser can't be emulated well enough to know
//! which of them is wrong. Kept in this browser, off until asked for. It
//! takes no taps and adds nothing to the page's size.

use super::{App, Document, HtmlElement, Window, standalone, viewport::Geometry};
use crate::{dom, ui::el};
use js_sys::Reflect;
use std::collections::VecDeque;
use std::fmt::Write as _;
use wasm_bindgen::JsCast as _;

/// Where this browser keeps whether they're shown.
const KEY: &str = "clackatoo.measures";

/// How many of the latest events are listed.
const LISTED: usize = 12;

/// Events further apart than this, in milliseconds, start a new count.
const APART: f64 = 3_000.0;

pub(super) struct Measures {
    shown: HtmlElement,
    /// Elements as tall as a CSS length, measured for what it comes to.
    probes: Vec<(&'static str, HtmlElement)>,
    /// When the events being counted from began, and the last came.
    began: f64,
    last: f64,
    events: VecDeque<String>,
}

impl Measures {
    fn new(document: &Document) -> Option<Self> {
        let body = document.body()?;
        let mut probes = Vec::new();
        for (name, height) in [
            ("vh", "100vh"),
            ("svh", "100svh"),
            ("dvh", "100dvh"),
            ("lvh", "100lvh"),
            ("inset-top", "env(safe-area-inset-top)"),
            ("inset-bottom", "var(--inset-bottom)"),
        ] {
            let probe: HtmlElement = dom::element_of(
                document,
                &el("div").class("measures-probe").style("height", height),
            )
            .unchecked_into();
            let _ = body.append_child(&probe);
            probes.push((name, probe));
        }
        let shown: HtmlElement = dom::element_of(
            document,
            &el("pre").class("measures").attr("aria-hidden", "true"),
        )
        .unchecked_into();
        let _ = body.append_child(&shown);
        Some(Self {
            shown,
            probes,
            began: 0.0,
            last: 0.0,
            events: VecDeque::new(),
        })
    }

    fn remove(&self) {
        self.shown.remove();
        for (_, probe) in &self.probes {
            probe.remove();
        }
    }

    /// `why` happened, with the page as `window` has it.
    fn note(&mut self, why: &str, window: &Window, document: &Document) {
        let now = js_sys::Date::now();
        if now - self.last > APART {
            self.began = now;
            if !self.events.is_empty() {
                self.events.push_back("--".to_owned());
            }
        }
        self.last = now;
        let mut line = format!("{:>5} {why:<13}", (now - self.began).round());
        if let Some(at) = Geometry::read(window, document) {
            let _ = write!(line, " h{:.0} top{:.0}", at.height, at.top);
        }
        let _ = write!(line, " y{:.0}", window.scroll_y().unwrap_or_default());
        self.events.push_back(line);
        while self.events.len() > LISTED {
            self.events.pop_front();
        }
    }

    fn show(&self, window: &Window, document: &Document) {
        let mut text = String::new();
        if let Some(at) = Geometry::read(window, document) {
            let page_top = window.visual_viewport().map_or(0.0, |v| v.page_top());
            let _ = writeln!(
                text,
                "visual  h {:.1} top {:.1} page {page_top:.1} x{:.2}",
                at.height, at.top, at.scale,
            );
            let _ = writeln!(text, "layout  inner {:.0} client {}", at.inner, at.client);
        } else {
            text.push_str("visual  none\n");
        }
        let screen = Reflect::get(window, &"screen".into())
            .and_then(|screen| Reflect::get(&screen, &"height".into()))
            .ok()
            .and_then(|height| height.as_f64())
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "window  scrollY {:.0} screen {screen:.0}{}",
            window.scroll_y().unwrap_or_default(),
            if standalone(window) {
                " home screen"
            } else {
                ""
            },
        );
        text.push_str("css    ");
        for (name, probe) in &self.probes {
            let _ = write!(text, " {name} {}", probe.offset_height());
        }
        text.push('\n');
        if let Ok(Some(page)) = document.query_selector(".chat-page, .term-page") {
            let rect = page.get_bounding_client_rect();
            let _ = write!(
                text,
                "page    top {:.1} bottom {:.1} h {:.1}",
                rect.top(),
                rect.bottom(),
                rect.height(),
            );
            // Which height was chosen: the large viewport's, or the
            // visual one's.
            if page.class_list().contains("tall") {
                text.push_str(" tall=lvh");
            } else {
                text.push_str(" =visual");
            }
            if page.class_list().contains("keyboard-open") {
                text.push_str(" keyboard-open");
            }
            text.push('\n');
        }
        if let Ok(Some(dock)) = document.query_selector(".composer, .keys-slot") {
            let _ = writeln!(
                text,
                "box     bottom {:.1}",
                dock.get_bounding_client_rect().bottom()
            );
        }
        for event in &self.events {
            text.push_str(event);
            text.push('\n');
        }
        self.shown.set_text_content(Some(text.trim_end()));
    }
}

impl App {
    /// Shows the measurements, or stops showing them, and keeps which in
    /// this browser.
    pub(super) fn toggle_measures(&mut self) {
        let storage = self.window.local_storage().ok().flatten();
        if let Some(measures) = self.viewport.measures.take() {
            measures.remove();
            if let Some(storage) = storage {
                let _ = storage.remove_item(KEY);
            }
            return;
        }
        if let Some(storage) = storage {
            let _ = storage.set_item(KEY, "on");
        }
        self.viewport.measures = Measures::new(&self.document);
        self.show_measures();
    }

    /// As the app starts: the measurements shown, if they were when it
    /// last ran here.
    pub(super) fn restore_measures(&mut self) {
        let on = (self.window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(KEY).ok().flatten())
            .is_some();
        if on && self.viewport.measures.is_none() {
            self.viewport.measures = Measures::new(&self.document);
            self.show_measures();
        }
    }

    /// Notes `why` in the measurements' list, if they're shown.
    pub(super) fn note_viewport(&mut self, why: &str) {
        if let Some(measures) = &mut self.viewport.measures {
            measures.note(why, &self.window, &self.document);
            measures.show(&self.window, &self.document);
        }
    }

    pub(super) fn show_measures(&self) {
        if let Some(measures) = &self.viewport.measures {
            measures.show(&self.window, &self.document);
        }
    }
}
