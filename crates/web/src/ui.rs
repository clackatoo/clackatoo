//! What a screen is made of, before it's made of the page's elements: a
//! tree of elements and text, built by plain functions of the app's
//! state, so what the app shows can be tested without a browser, and
//! made into the page's own elements in one place (see `dom`).
//!
//! Text is only ever text: it becomes text nodes, never HTML, so nothing
//! an agent writes can put markup, or a script, in the page. Attributes
//! are named by the app alone.

use crate::action::Action;

/// A node: an element, or text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    Text(String),
}

/// An element: its tag, attributes and children.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    pub tag: &'static str,
    pub attrs: Vec<(&'static str, String)>,
    pub children: Vec<Node>,
}

/// A new element with `tag`.
#[must_use]
pub fn el(tag: &'static str) -> Element {
    Element {
        tag,
        ..Element::default()
    }
}

impl Element {
    /// The same, with `class` among its classes.
    #[must_use]
    pub fn class(mut self, class: &str) -> Self {
        match self.attrs.iter_mut().find(|(name, _)| *name == "class") {
            Some((_, classes)) => {
                classes.push(' ');
                classes.push_str(class);
            }
            None => self.attrs.push(("class", class.to_owned())),
        }
        self
    }

    /// The same, with `class` among its classes if `when`.
    #[must_use]
    pub fn class_if(self, when: bool, class: &str) -> Self {
        if when { self.class(class) } else { self }
    }

    /// The same, with the attribute `name` set to `value`.
    #[must_use]
    pub fn attr(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.attrs.push((name, value.into()));
        self
    }

    /// The same, doing `action` when tapped.
    #[must_use]
    pub fn action(self, action: &Action) -> Self {
        self.attr("data-action", action.encode())
    }

    /// The same, with `child` after its children.
    #[must_use]
    pub fn child(mut self, child: impl Into<Node>) -> Self {
        self.children.push(child.into());
        self
    }

    /// The same, with `children` after its children.
    #[must_use]
    pub fn children(mut self, children: impl IntoIterator<Item = impl Into<Node>>) -> Self {
        self.children.extend(children.into_iter().map(Into::into));
        self
    }

    /// The same, with `text` after its children.
    #[must_use]
    pub fn text(self, text: impl Into<String>) -> Self {
        self.child(Node::Text(text.into()))
    }

    /// The value of its attribute `name`, if it has it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        (self.attrs.iter())
            .find(|(attr, _)| *attr == name)
            .map(|(_, value)| value.as_str())
    }

    /// All the text in it, for tests.
    #[must_use]
    pub fn all_text(&self) -> String {
        let mut text = String::new();
        for child in &self.children {
            match child {
                Node::Text(more) => text.push_str(more),
                Node::Element(element) => text.push_str(&element.all_text()),
            }
        }
        text
    }

    /// Its descendants with the attribute `name`, depth first, for tests.
    #[must_use]
    pub fn find_all(&self, name: &str) -> Vec<&Element> {
        let mut found = Vec::new();
        for child in &self.children {
            if let Node::Element(element) = child {
                if element.get(name).is_some() {
                    found.push(element);
                }
                found.extend(element.find_all(name));
            }
        }
        found
    }
}

impl From<Element> for Node {
    fn from(element: Element) -> Self {
        Self::Element(element)
    }
}

impl From<&str> for Node {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for Node {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_are_built_up() {
        let tree = el("div")
            .class("a")
            .class("b")
            .class_if(false, "c")
            .attr("data-x", "1")
            .child(el("p").text("hello "))
            .text("world");
        assert_eq!(tree.get("class"), Some("a b"));
        assert_eq!(tree.all_text(), "hello world");
        assert_eq!(tree.find_all("data-x").len(), 0, "only descendants");
    }
}
