//! The page showing a terminal: its header, its rows, the history above
//! them, the compact key row, and an invisible input that takes the phone's
//! keyboard.
//!
//! Rows are the page's own elements, one per row, and only rows that
//! changed are made again: a cursor that moved, two. A terminal of 200
//! columns by 60 rows changing ten times a second touches at most sixty
//! rows each time, which a phone lays out well within a frame, and the
//! text stays text, to select and copy.
//!
//! The rows are drawn at the terminal's own size, the default font kept
//! between [`MIN_FONT`] and 16 pixels: a wider
//! terminal pans sideways instead. Pinching zooms, for as long as the
//! terminal is shown.

use clackatoo_protocol::Size;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Document, Element as DomElement, Event, HtmlElement, HtmlInputElement};

use crate::action::Action;
use crate::dom;
use crate::grid::{self, Cursor};
use crate::state::State;
use crate::term::OpenTerminal;
use crate::ui::{Element, el};
use crate::view;

/// The smallest font rows are drawn in to fit the phone's width, in CSS
/// pixels: a wider terminal pans sideways rather than go smaller.
pub(super) const MIN_FONT: f64 = 14.0;

/// The font a terminal fitted to the phone is sized for, unless the user
/// pinched to another.
const FIT_FONT: f64 = 14.0;

/// The smallest and largest fonts pinching gets to.
const PINCH_FONTS: (f64, f64) = (10.0, 32.0);

/// A row's height over its font's size: `.grid`'s `line-height`.
const LINE_HEIGHT: f64 = 1.45;

/// What the box typed into always holds, and nothing else: a zero-width
/// space, so Backspace on a phone's keyboard has something to delete,
/// and says so, however empty the box looks.
pub(super) const SENTINEL: &str = "\u{200B}";

/// Puts back what the box typed into always holds, with the caret after
/// it.
pub(super) fn keep_sentinel(input: &HtmlInputElement) {
    if input.value() != SENTINEL {
        input.set_value(SENTINEL);
    }
    // After it, in UTF-16 code units, as the browser counts.
    let after = u32::try_from(SENTINEL.encode_utf16().count()).unwrap_or(1);
    let _ = input.set_selection_range(after, after);
}

/// The page's parts, and how its rows are drawn.
pub(super) struct TermPage {
    main: HtmlElement,
    header: DomElement,
    pub(super) scroller: HtmlElement,
    grid: HtmlElement,
    history: DomElement,
    screen: DomElement,
    keys: DomElement,
    pub(super) input: HtmlInputElement,
    /// What the header and the keys last showed, so they're only made
    /// again when they change.
    shown_header: Element,
    shown_keys: Element,
    /// A cell's width over the font's size, measured once.
    ratio: f64,
    /// The font the rows are drawn in, in CSS pixels.
    font: f64,
    /// The columns the rows are drawn at.
    cols: u16,
    /// The font the user pinched to, kept while the terminal is shown.
    zoomed: Option<f64>,
    /// A pinch going on: how far apart the fingers started, and the font
    /// then.
    pinch: Option<(f64, f64)>,
    /// Whether anything has been drawn.
    drawn: bool,
    controls_open: bool,
    /// What hears the fingers on the rows and the keys, for as long as
    /// the page is shown.
    _touches: Vec<Closure<dyn FnMut(Event)>>,
}

impl TermPage {
    /// The page, empty, in place of what `root` showed.
    pub(super) fn new(document: &Document, root: &DomElement) -> Self {
        let make = |element: &Element| dom::element_of(document, element);
        let main: HtmlElement = make(&el("main").class("term-page")).unchecked_into();
        let header = make(&el("div").class("header-slot"));
        // Tapping the rows brings up the keyboard.
        let scroller: HtmlElement =
            make(&el("div").class("term").action(&crate::action::Action::Type)).unchecked_into();
        let grid: HtmlElement = make(&el("div").class("grid")).unchecked_into();
        let history = make(&el("div").class("history"));
        let screen = make(&el("div").class("screen"));
        let keys = make(&el("div").class("keys-slot"));
        // What's typed goes as it's typed, so nothing the phone's keyboard
        // does to words may touch it.
        let input: HtmlInputElement = make(
            &el("input")
                .attr("id", "keys")
                .class("terminal-input")
                .attr("type", "text")
                .attr("autocomplete", "off")
                .attr("autocorrect", "off")
                .attr("autocapitalize", "off")
                .attr("spellcheck", "false")
                .attr("enterkeyhint", "send")
                .attr("aria-label", "Type into the terminal")
                .attr("value", SENTINEL),
        )
        .unchecked_into();
        let _ = grid.append_child(&history);
        let _ = grid.append_child(&screen);
        let _ = scroller.append_child(&grid);
        let _ = main.append_child(&input);
        for part in [&header, &scroller, &keys] {
            let _ = main.append_child(part);
        }
        root.set_text_content(None);
        let _ = root.append_child(&main);
        let ratio = measure(document, &grid);
        let touches = super::touch_listeners(&scroller, &main);
        Self {
            main,
            header,
            scroller,
            grid,
            history,
            screen,
            keys,
            input,
            shown_header: el("div"),
            shown_keys: el("div"),
            ratio,
            font: 0.0,
            cols: 0,
            zoomed: None,
            pinch: None,
            drawn: false,
            controls_open: false,
            _touches: touches,
        }
    }

    /// Draws what changed in `term`, as `state` has it.
    pub(super) fn draw(&mut self, document: &Document, state: &State, term: &OpenTerminal) {
        let fresh = !self.drawn;
        let header = view::terminal_header(state, term);
        if fresh || header != self.shown_header {
            dom::replace_children(document, &self.header, &el("div").child(header.clone()));
            self.shown_header = header;
        }
        let keys = view::key_row(state, term);
        if fresh || keys != self.shown_keys {
            dom::replace_children(document, &self.keys, &el("div").child(keys.clone()));
            self.shown_keys = keys;
        }
        self.sync_controls();
        let Some(grid) = term.grid() else {
            if fresh {
                let opening = el("p").class("muted").text("Opening…");
                self.screen.set_text_content(None);
                let _ = self
                    .screen
                    .append_child(&dom::element_of(document, &opening));
            }
            return;
        };
        self.drawn = true;
        let at_bottom = self.at_bottom();
        let dirty = &term.dirty;
        if dirty.all || grid.size.cols() != self.cols {
            self.cols = grid.size.cols();
            let _ = (self.grid.style()).set_property("--cols", &self.cols.to_string());
            self.fit_font();
        }
        let cols = usize::from(self.cols);
        let cursor = grid.state.cursor;
        let row = |index: usize| {
            let on = (cursor.visible && usize::from(cursor.line) == index).then_some(Cursor {
                col: cursor.col,
                shape: cursor.shape,
            });
            let failed = u16::try_from(index).ok().and_then(|row| term.failed(row));
            dom::element_of(document, &grid::row(&grid.lines[index], cols, on, failed))
        };
        if dirty.all || self.screen.child_element_count() as usize != grid.lines.len() {
            self.screen.set_text_content(None);
            for index in 0..grid.lines.len() {
                let _ = self.screen.append_child(&row(index));
            }
        } else {
            let children = self.screen.children();
            for &index in &dirty.rows {
                let index = usize::from(index);
                if index >= grid.lines.len() {
                    continue;
                }
                if let Some(old) = children.item(u32::try_from(index).unwrap_or(u32::MAX)) {
                    let _ = self.screen.replace_child(&row(index), &old);
                }
            }
        }
        self.draw_history(document, term, at_bottom);
        let _ = (self.history.class_list()).toggle_with_force("hidden", grid.state.alternate);
        if at_bottom || fresh {
            self.to_bottom();
        }
    }

    /// Draws what was added to the history held, or throws away what was
    /// drawn, keeping what's on screen in place.
    fn draw_history(&self, document: &Document, term: &OpenTerminal, at_bottom: bool) {
        let history = &term.history;
        if history.cleared {
            self.history.set_text_content(None);
        }
        // Line `index` of those held.
        let row = |index: usize| {
            let line = &history.lines[index];
            let failed = history.failed(history.first + index as u64);
            let row = grid::row(&line.line, usize::from(line.width), None, failed);
            dom::element_of(document, &row)
        };
        // Lines drawn before whose marks changed, as when a command ends
        // after its prompt has scrolled into history. Those added since
        // are drawn as they are now below.
        let drawn_before = history.added_above..history.lines.len() - history.added_below;
        if !history.cleared {
            let children = self.history.children();
            for &line in &term.dirty.history_marks {
                let Some(index) = (line.checked_sub(history.first))
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| drawn_before.contains(index))
                else {
                    continue;
                };
                // Among the children as they were, before those added above.
                let drawn = index - history.added_above;
                if let Some(old) = children.item(u32::try_from(drawn).unwrap_or(u32::MAX)) {
                    let _ = self.history.replace_child(&row(index), &old);
                }
            }
        }
        if history.added_above > 0 {
            let before = self.scroller.scroll_height();
            let first = self.history.first_child();
            for index in 0..history.added_above {
                let _ = self.history.insert_before(&row(index), first.as_ref());
            }
            if !at_bottom {
                let grown = self.scroller.scroll_height() - before;
                self.scroller
                    .set_scroll_top(self.scroller.scroll_top() + grown);
            }
        }
        if history.added_below > 0 {
            for index in history.lines.len() - history.added_below..history.lines.len() {
                let _ = self.history.append_child(&row(index));
            }
        }
    }

    /// Whether the rows are scrolled to their foot, give or take a row.
    fn at_bottom(&self) -> bool {
        let scroller = &self.scroller;
        f64::from(scroller.scroll_top() + scroller.client_height())
            >= f64::from(scroller.scroll_height()) - self.font * LINE_HEIGHT
    }

    /// Scrolls to the foot of the rows: the live screen's last row.
    fn to_bottom(&self) {
        self.scroller.set_scroll_top(self.scroller.scroll_height());
    }

    /// Whether the user has scrolled near the top of what's held, where
    /// older history is wanted.
    pub(super) fn near_top(&self) -> bool {
        self.scroller.scroll_top() < self.scroller.client_height()
    }

    /// Sizes the font so the terminal's width fits the phone's, but not
    /// under [`MIN_FONT`], unless the user pinched to another.
    pub(super) fn fit_font(&mut self) {
        // Nothing drawn yet to fit.
        if self.cols == 0 {
            return;
        }
        let fits = self.width() / (f64::from(self.cols.max(1)) * self.ratio);
        // Use whole pixels so a fitted grid does not grow taller than the
        // space calculated with FIT_FONT through fractional rounding.
        let font = self.zoomed.unwrap_or(fits.floor().clamp(MIN_FONT, 16.0));
        self.set_font(font);
    }

    /// Draws the rows in a `font` of this size.
    fn set_font(&mut self, font: f64) {
        if (font - self.font).abs() < 0.01 {
            return;
        }
        let at_bottom = self.at_bottom();
        self.font = font;
        self.sync_controls();
        let _ = (self.grid.style()).set_property("font-size", &format!("{font:.2}px"));
        if at_bottom {
            self.to_bottom();
        }
    }

    /// The width rows have, inside the scroller's padding.
    fn width(&self) -> f64 {
        f64::from(self.scroller.client_width() - 24).max(1.0)
    }

    /// The size the terminal would be to fit the phone: as many cells of
    /// [`FIT_FONT`], or the font pinched to, as fit the rows' space.
    pub(super) fn phone_size(&self) -> Option<Size> {
        let font = self.zoomed.unwrap_or(FIT_FONT);
        let height = f64::from(self.scroller.client_height() - 32).max(1.0);
        let fit = |space: f64, cell: f64| {
            let cells = (space / cell).floor().clamp(2.0, f64::from(u16::MAX));
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "whole, clamped to what fits"
            )]
            let cells = cells as u16;
            cells
        };
        let cols = fit(self.width(), font * self.ratio);
        let rows = fit(height, font * LINE_HEIGHT);
        Size::new(cols, rows).ok()
    }

    /// Two fingers came down on the rows, `apart` pixels apart: a pinch
    /// starts.
    pub(super) fn pinch_started(&mut self, apart: f64) {
        self.pinch = Some((apart.max(1.0), self.font));
    }

    /// The fingers of a pinch are now `apart` pixels apart: the font
    /// grows or shrinks with them. Returns whether a pinch goes on.
    pub(super) fn pinched(&mut self, apart: f64) -> bool {
        let Some((from, font)) = self.pinch else {
            return false;
        };
        let font = (font * apart / from).clamp(PINCH_FONTS.0, PINCH_FONTS.1);
        self.zoomed = Some(font);
        self.set_font(font);
        true
    }

    /// The pinch ended.
    pub(super) fn pinch_ended(&mut self) {
        self.pinch = None;
    }

    pub(super) fn toggle_controls(&mut self) {
        self.controls_open = !self.controls_open;
        self.sync_controls();
    }

    pub(super) fn change_font(&mut self, step: Option<f64>) {
        self.zoomed = step.map(|step| (self.font + step).clamp(PINCH_FONTS.0, PINCH_FONTS.1));
        self.fit_font();
    }

    fn sync_controls(&self) {
        let _ = self
            .main
            .class_list()
            .toggle_with_force("controls-open", self.controls_open);
        if let Ok(Some(toggle)) = self.header.query_selector(".term-controls-toggle") {
            let _ = toggle.set_attribute(
                "aria-expanded",
                if self.controls_open { "true" } else { "false" },
            );
        }
        if let Ok(Some(size)) = self.keys.query_selector(".terminal-font-size") {
            size.set_text_content(Some(&format!("{:.0}", self.font)));
        }
    }

    /// Puts the page where the phone shows it, above its keyboard: the
    /// visual viewport, `height` pixels tall, `top` pixels down the page.
    pub(super) fn place(&self, top: f64, height: f64) {
        let at_bottom = self.at_bottom();
        let typing = self
            .input
            .owner_document()
            .and_then(|d| d.active_element())
            .is_some_and(|active| active == *self.input.unchecked_ref::<DomElement>());
        let _ = self.main.class_list().toggle_with_force("typing", typing);
        let style = self.main.style();
        let _ = style.set_property("top", &format!("{top}px"));
        let _ = style.set_property("height", &format!("{height}px"));
        if self.drawn && at_bottom {
            self.to_bottom();
        }
    }
}

/// A cell's width over its font's size, for `.grid`'s font: ten zeros in
/// a 100px font, measured.
fn measure(document: &Document, grid: &HtmlElement) -> f64 {
    let probe = dom::element_of(document, &el("span").class("probe").text("0000000000"));
    let _ = grid.append_child(&probe);
    let width = probe
        .dyn_ref::<HtmlElement>()
        .map_or(0, HtmlElement::offset_width);
    probe.remove();
    if width > 0 {
        f64::from(width) / 1000.0
    } else {
        // Nothing laid out yet: monospace fonts are about this.
        0.6
    }
}

impl super::App {
    pub(super) fn terminal_control(&mut self, action: Action) {
        match action {
            Action::Fit => {
                let size = self.term_page.as_ref().and_then(TermPage::phone_size);
                if size.is_some() {
                    self.state.fit(size);
                }
            }
            Action::Unfit => self.state.fit(None),
            Action::Type => {
                if let Some(page) = &self.term_page {
                    let _ = page.input.focus();
                }
            }
            Action::HideKeyboard => {
                if let Some(page) = &self.term_page {
                    let _ = page.input.blur();
                }
                self.viewport_settling();
            }
            Action::TerminalControls => {
                if let Some(page) = &mut self.term_page {
                    page.toggle_controls();
                }
                self.viewport_changed();
            }
            Action::SmallerText | Action::LargerText | Action::ResetText => {
                if let Some(page) = &mut self.term_page {
                    page.change_font(match action {
                        Action::SmallerText => Some(-1.0),
                        Action::LargerText => Some(1.0),
                        _ => None,
                    });
                }
                self.viewport_changed();
            }
            _ => {}
        }
    }
}
