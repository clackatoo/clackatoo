//! [`ui`](crate::ui) trees made into the page's own elements.
//!
//! Text becomes text nodes and attributes are set one by one, never
//! `innerHTML`, so nothing from the daemon is ever read as markup.

use wasm_bindgen::JsCast as _;
use web_sys::{Document, Element as DomElement};

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
