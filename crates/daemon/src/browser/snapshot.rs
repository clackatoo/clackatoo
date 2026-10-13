//! A page as an agent reads it: its accessibility tree, as Chromium
//! gives it (`Accessibility.getFullAXTree`), written out as an indented
//! outline, one line a thing on the page, each with a ref the agent
//! clicks or types into it by.
//!
//! ```text
//! - document "Example Domain" [ref=1]
//!   - heading "Example Domain" [level=1] [ref=4]
//!   - paragraph [ref=5]
//!     - text "This domain is for use in documentation examples."
//!   - link "Learn more" [ref=8]
//! ```
//!
//! A ref is the node's `backendNodeId`, which Chromium keeps for as long
//! as the node lives, so it needs no table of its own, and one taken from
//! an old snapshot works as long as the thing it names is still there.
//! Containers with no name or role of their own are left out, and their
//! insides moved up; text that only repeats the name of what it's in is
//! left out too.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::rc::Rc;

use serde_json::Value;

/// The most of a name or a value written, in characters.
const MAX_NAME: usize = 200;

/// Roles that say nothing themselves: their insides are written in their
/// place.
const SILENT: &[&str] = &[
    "none",
    "generic",
    "GenericContainer",
    "LayoutTable",
    "LayoutTableRow",
    "LayoutTableCell",
    "LineBreak",
    "Ignored",
    "IgnoredRole",
    "presentation",
];

/// How deep in the tree the outline goes: what's deeper is left out.
const MAX_DEPTH: usize = 256;

/// The outline of the tree `nodes`, at most `limit` bytes: past that, it
/// says how many lines were left out. The tree is walked without
/// recursion, as deep as [`MAX_DEPTH`], each node once, so no page's tree
/// can take the daemon down, however deep, or even if it loops.
pub(super) fn outline(nodes: &[Value], limit: usize) -> String {
    let by_id: HashMap<&str, &Value> = (nodes.iter())
        .filter_map(|node| Some((node["nodeId"].as_str()?, node)))
        .collect();
    let mut writer = Writer {
        out: String::new(),
        limit,
        left_out: 0,
    };
    // The root is the node with no parent; Chromium lists it first.
    let root = nodes.iter().find(|node| node.get("parentId").is_none());
    let mut seen: HashSet<&str> = HashSet::new();
    let mut left: Vec<(&Value, usize, usize, Rc<str>)> = (root.into_iter())
        .map(|root| (root, 0, 0, Rc::from("")))
        .collect();
    let mut too_deep = false;
    while let Some((node, levels, depth, around)) = left.pop() {
        if let Some(id) = node["nodeId"].as_str()
            && !seen.insert(id)
        {
            continue;
        }
        if levels > MAX_DEPTH {
            too_deep = true;
            continue;
        }
        let Some((inner_depth, inner_around)) = writer.write(node, depth, &around) else {
            continue;
        };
        let children = node["childIds"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        // Last first, so the first comes off the stack first.
        for child in children.iter().rev() {
            if let Some(&child) = child.as_str().and_then(|id| by_id.get(id)) {
                let around = inner_around.clone().unwrap_or_else(|| Rc::clone(&around));
                left.push((child, levels + 1, inner_depth, around));
            }
        }
    }
    let Writer {
        mut out, left_out, ..
    } = writer;
    if too_deep {
        let _ = writeln!(out, "… and what's more than {MAX_DEPTH} levels deep");
    }
    if left_out > 0 {
        let _ = writeln!(
            out,
            "… and {left_out} more lines: browser_read gives the page's text whole"
        );
    }
    if out.is_empty() {
        out.push_str("(the page is empty)\n");
    }
    out
}

/// Writes nodes out, keeping count of what didn't fit.
struct Writer {
    out: String,
    limit: usize,
    left_out: usize,
}

impl Writer {
    /// Writes `node` at `depth`. `around` is the name of what it's in,
    /// whose repeats are left out. Returns the depth its insides go at,
    /// and their `around` if it's another, or `None` if they're left out.
    fn write(
        &mut self,
        node: &Value,
        depth: usize,
        around: &str,
    ) -> Option<(usize, Option<Rc<str>>)> {
        let role = text_of(&node["role"]);
        let name = text_of(&node["name"]);
        let ignored = node["ignored"].as_bool().unwrap_or(false);
        let mut inner_depth = depth;
        let mut inner_around = None;
        match role {
            _ if ignored => {}
            "InlineTextBox" => return None,
            "StaticText" => {
                let text = name.trim();
                if !text.is_empty() && !around.contains(text) {
                    let line = format!("- text \"{}\"", clipped(text));
                    self.line(depth, &line);
                }
                return None;
            }
            role if SILENT.contains(&role) && name.trim().is_empty() => {}
            role => {
                let mut line = format!("- {}", shown_role(role));
                if !name.trim().is_empty() {
                    let _ = write!(line, " \"{}\"", clipped(name.trim()));
                    inner_around = Some(Rc::from(name));
                }
                let value = text_of(&node["value"]);
                if !value.is_empty() && value != name {
                    let _ = write!(line, ": \"{}\"", clipped(value));
                }
                for state in states(node) {
                    let _ = write!(line, " [{state}]");
                }
                if let Some(id) = node["backendDOMNodeId"].as_u64() {
                    let _ = write!(line, " [ref={id}]");
                }
                self.line(depth, &line);
                inner_depth = depth + 1;
            }
        }
        Some((inner_depth, inner_around))
    }

    fn line(&mut self, depth: usize, line: &str) {
        if self.left_out > 0 || self.out.len() + line.len() + depth * 2 > self.limit {
            self.left_out += 1;
            return;
        }
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(line);
        self.out.push('\n');
    }
}

/// The text of an `AXValue`, or nothing.
fn text_of(value: &Value) -> &str {
    value["value"].as_str().unwrap_or_default()
}

/// What `role` is called in the outline: the page itself is a document.
fn shown_role(role: &str) -> &str {
    match role {
        "RootWebArea" | "WebArea" => "document",
        role => role,
    }
}

/// The states worth saying of `node`: whether it's checked, disabled,
/// focused and so on, and a heading's level.
fn states(node: &Value) -> Vec<String> {
    let mut states = Vec::new();
    let properties = node["properties"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for property in properties {
        let name = property["name"].as_str().unwrap_or_default();
        let value = &property["value"]["value"];
        match name {
            "level" => {
                if let Some(level) = value.as_u64() {
                    states.push(format!("level={level}"));
                }
            }
            "checked" | "pressed" => match value.as_str() {
                Some("true") => states.push(name.to_owned()),
                Some("mixed") => states.push(format!("{name}=mixed")),
                _ => {}
            },
            "disabled" | "focused" | "selected" | "expanded" | "required" | "invalid"
                if value.as_bool() == Some(true) || value.as_str() == Some("true") =>
            {
                states.push(name.to_owned());
            }
            _ => {}
        }
    }
    states
}

/// `text` on one line, cut to [`MAX_NAME`] characters, with quotes
/// escaped.
fn clipped(text: &str) -> String {
    let mut clipped: String = (text.chars().take(MAX_NAME))
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if text.chars().nth(MAX_NAME).is_some() {
        clipped.push('…');
    }
    clipped.replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn node(id: &str, parent: Option<&str>, role: &str, name: &str, children: &[&str]) -> Value {
        let mut node = json!({
            "nodeId": id,
            "ignored": false,
            "role": { "type": "role", "value": role },
            "name": { "type": "computedString", "value": name },
            "childIds": children,
            "backendDOMNodeId": id.parse::<u64>().unwrap() + 100,
        });
        if let Some(parent) = parent {
            node["parentId"] = json!(parent);
        }
        node
    }

    #[test]
    fn the_outline_leaves_out_what_says_nothing() {
        let mut heading = node("3", Some("2"), "heading", "Hello", &["4"]);
        heading["properties"] =
            json!([{ "name": "level", "value": { "type": "integer", "value": 1 } }]);
        let mut box_ = node("7", Some("2"), "textbox", "Name", &[]);
        box_["value"] = json!({ "type": "string", "value": "Ada" });
        box_["properties"] = json!([{ "name": "focused", "value": { "type": "booleanOrUndefined", "value": true } }]);
        let nodes = [
            node("1", None, "RootWebArea", "Page", &["2"]),
            node("2", Some("1"), "generic", "", &["3", "5", "7"]),
            heading,
            node("4", Some("3"), "StaticText", "Hello", &["9"]),
            node("5", Some("2"), "paragraph", "", &["6"]),
            node("6", Some("5"), "StaticText", "Some \"quoted\" words", &[]),
            box_,
            node("9", Some("4"), "InlineTextBox", "Hello", &[]),
        ];
        assert_eq!(
            outline(&nodes, 10_000),
            "- document \"Page\" [ref=101]\n\
            \x20 - heading \"Hello\" [level=1] [ref=103]\n\
            \x20 - paragraph [ref=105]\n\
            \x20   - text \"Some \\\"quoted\\\" words\"\n\
            \x20 - textbox \"Name\": \"Ada\" [focused] [ref=107]\n"
        );
    }

    #[test]
    fn a_tree_however_deep_or_looping_is_outlined_without_recursion() {
        // Far deeper than any stack would take, one node in the next.
        let depth = 200_000;
        let mut nodes = Vec::with_capacity(depth);
        for id in 1..=depth {
            let children: Vec<String> = if id < depth {
                vec![(id + 1).to_string()]
            } else {
                Vec::new()
            };
            let children: Vec<&str> = children.iter().map(String::as_str).collect();
            let parent = (id - 1).to_string();
            let parent = (id > 1).then_some(parent.as_str());
            nodes.push(node(&id.to_string(), parent, "group", "g", &children));
        }
        let deep = outline(&nodes, 1_000_000);
        assert!(
            deep.ends_with("… and what's more than 256 levels deep\n"),
            "{deep}"
        );
        assert_eq!(deep.lines().count(), MAX_DEPTH + 2);
        // A node that's its own child, and two that are each other's.
        let looping = [
            node("1", None, "list", "a", &["2"]),
            node("2", Some("1"), "listitem", "b", &["1", "2", "3"]),
            node("3", Some("2"), "listitem", "c", &["2"]),
        ];
        assert_eq!(
            outline(&looping, 10_000),
            "- list \"a\" [ref=101]\n  - listitem \"b\" [ref=102]\n    - listitem \"c\" [ref=103]\n"
        );
    }

    #[test]
    fn a_long_outline_says_what_it_left_out() {
        let mut nodes = vec![node("1", None, "RootWebArea", "Page", &[])];
        let children: Vec<String> = (2..50).map(|id| id.to_string()).collect();
        nodes[0]["childIds"] = json!(children);
        for id in &children {
            nodes.push(node(id, Some("1"), "link", "A link somewhere", &[]));
        }
        let outline = outline(&nodes, 200);
        assert!(outline.len() < 300, "{outline}");
        assert!(outline.ends_with("more lines: browser_read gives the page's text whole\n"));
        assert!(outline.starts_with("- document \"Page\""));
    }
}
