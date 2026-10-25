//! [`ui`](crate::ui) trees made into the page's own elements.
//!
//! Text becomes text nodes and attributes are set one by one, never
//! `innerHTML`, so nothing from the daemon is ever read as markup.

use wasm_bindgen::JsCast as _;
use web_sys::{Document, Element as DomElement, HtmlElement};

use crate::ui::{Element, Node};

/// `node`, made in `document`.
pub(crate) fn build(document: &Document, node: &Node) -> web_sys::Node {
    match node {
        Node::Text(text) => document.create_text_node(text).unchecked_into(),
        Node::Element(element) => element_of(document, element).unchecked_into(),
    }
}

/// `element`, made in `document`.
pub(crate) fn element_of(document: &Document, element: &Element) -> DomElement {
    let made = document
        .create_element(element.tag)
        .expect("the app's tags are elements");
    for (name, value) in &element.attrs {
        // `value` and `selected` are properties too, which an element's
        // first drawing takes from the attributes.
        let _ = made.set_attribute(name, value);
    }
    if !element.styles.is_empty()
        && let Some(html) = made.dyn_ref::<HtmlElement>()
    {
        let style = html.style();
        for (name, value) in &element.styles {
            let _ = style.set_property(name, value);
        }
    }
    if let Some(preview) = &element.image {
        let canvas: web_sys::HtmlCanvasElement = made.clone().unchecked_into();
        canvas.set_width(preview.width);
        canvas.set_height(preview.height);
        if let Ok(Some(context)) = canvas.get_context("2d")
            && let Ok(context) = context.dyn_into::<web_sys::CanvasRenderingContext2d>()
            && let Ok(data) = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
                wasm_bindgen::Clamped(preview.rgba.as_ref()),
                preview.width,
                preview.height,
            )
        {
            let _ = context.put_image_data(&data, 0.0, 0.0);
        }
    }
    for child in &element.children {
        let _ = made.append_child(&build(document, child));
    }
    made
}

/// Puts `element`, made in `document`, in place of `old`'s children.
pub(crate) fn replace_children(document: &Document, old: &DomElement, element: &Element) {
    old.set_text_content(None);
    for child in &element.children {
        let _ = old.append_child(&build(document, child));
    }
}

/// Keep a settings search usable while results or provider data change.
pub(crate) fn replace_panel(document: &Document, old: &DomElement, element: &Element) {
    let active = document.active_element().filter(|e| old.contains(Some(e)));
    let selection = active
        .as_ref()
        .and_then(|e| e.dyn_ref::<web_sys::HtmlInputElement>())
        .and_then(|e| Some((e.selection_start().ok()??, e.selection_end().ok()??)));
    let focus = active.as_ref().and_then(|e| {
        [
            "id",
            "data-favorite",
            "data-model-provider",
            "data-picker-tab",
            "data-pick",
        ]
        .into_iter()
        .find_map(|name| {
            e.get_attribute(name)
                .filter(|v| !v.is_empty())
                .map(|value| (name, value))
        })
    });
    let scrolls: Vec<_> = [".sheet-body", ".model-list", ".provider-tabs"]
        .into_iter()
        .filter_map(|selector| {
            old.query_selector(selector)
                .ok()
                .flatten()
                .map(|e| (selector, e.scroll_top(), e.scroll_left()))
        })
        .collect();
    replace_children(document, old, element);
    if let Some((attr, value)) = focus
        && let Ok(items) = old.query_selector_all(&format!("[{attr}]"))
    {
        for index in 0..items.length() {
            if let Some(e) = items
                .item(index)
                .and_then(|n| n.dyn_into::<HtmlElement>().ok())
                && e.get_attribute(attr).as_deref() == Some(&value)
            {
                let _ = e.focus();
                if let Some((start, end)) = selection
                    && let Some(input) = e.dyn_ref::<web_sys::HtmlInputElement>()
                {
                    let _ = input.set_selection_range(start, end);
                }
                break;
            }
        }
    }
    for (selector, top, left) in scrolls {
        if let Ok(Some(e)) = old.query_selector(selector) {
            e.set_scroll_top(top);
            e.set_scroll_left(left);
        }
    }
}
