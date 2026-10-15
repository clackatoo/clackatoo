//! The agents' words, read as Markdown, as elements a phone lays out:
//! paragraphs, headings, lists, quotes, tables, and code, which scrolls
//! sideways rather than wrapping.
//!
//! HTML in the Markdown is shown as the text it is, never as markup, and
//! a link goes only to the web or mail: `javascript:` and the like stay
//! text. Images aren't loaded, which the page's policy forbids anyway:
//! they're links.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::ui::{Element, Node, el};

/// `markdown` as elements.
#[must_use]
pub fn render(markdown: &str) -> Vec<Node> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // The innermost element being built is last; the first holds them all.
    let mut stack: Vec<Element> = vec![el("div")];
    // Whether the table being read is in its head, whose cells are `th`,
    // how its columns line up, and the column of the cell next.
    let mut in_head = false;
    let mut aligns: Vec<Alignment> = Vec::new();
    let mut column = 0;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => {
                let element = match tag {
                    Tag::Paragraph => el("p"),
                    Tag::Heading { level, .. } => el(heading(level)),
                    Tag::BlockQuote(_) => el("blockquote"),
                    Tag::CodeBlock(kind) => {
                        let code = match kind {
                            CodeBlockKind::Fenced(language) if !language.is_empty() => {
                                el("pre").attr("data-language", language.to_string())
                            }
                            _ => el("pre"),
                        };
                        code.class("code")
                    }
                    Tag::List(Some(start)) if start != 1 => {
                        el("ol").attr("start", start.to_string())
                    }
                    Tag::List(Some(_)) => el("ol"),
                    Tag::List(None) => el("ul"),
                    Tag::Item => el("li"),
                    Tag::Emphasis => el("em"),
                    Tag::Strong => el("strong"),
                    Tag::Strikethrough => el("del"),
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => link(&dest_url),
                    Tag::Table(columns) => {
                        aligns = columns;
                        el("table")
                    }
                    Tag::TableHead => {
                        in_head = true;
                        column = 0;
                        el("tr")
                    }
                    Tag::TableRow => {
                        column = 0;
                        el("tr")
                    }
                    Tag::TableCell => {
                        let cell = el(if in_head { "th" } else { "td" });
                        match aligns.get(column) {
                            Some(Alignment::Center) => cell.class("middle"),
                            Some(Alignment::Right) => cell.class("right"),
                            _ => cell,
                        }
                    }
                    _ => el("span"),
                };
                stack.push(element);
            }
            Event::End(end) => {
                match end {
                    TagEnd::TableHead => in_head = false,
                    TagEnd::TableCell => column += 1,
                    _ => {}
                }
                close(&mut stack);
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                push(&mut stack, Node::Text(text.into_string()));
            }
            Event::Code(code) => push(&mut stack, el("code").text(code.into_string()).into()),
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                push(&mut stack, el("code").text(math.into_string()).into());
            }
            Event::SoftBreak => push(&mut stack, Node::Text("\n".to_owned())),
            Event::HardBreak => push(&mut stack, el("br").into()),
            Event::Rule => push(&mut stack, el("hr").into()),
            Event::TaskListMarker(done) => {
                push(
                    &mut stack,
                    Node::Text(if done { "☑ " } else { "☐ " }.to_owned()),
                );
            }
            Event::FootnoteReference(name) => {
                push(&mut stack, Node::Text(format!("[{name}]")));
            }
        }
    }
    // Anything left open, as from Markdown still streaming in, is closed.
    while stack.len() > 1 {
        close(&mut stack);
    }
    stack
        .pop()
        .map(|outermost| outermost.children)
        .unwrap_or_default()
}

/// Closes the innermost element being built, unless it's the outermost,
/// adding it to the one it's in.
fn close(stack: &mut Vec<Element>) {
    if stack.len() > 1
        && let Some(done) = stack.pop()
    {
        push(stack, done.into());
    }
}

/// Adds `node` to the innermost element being built.
fn push(stack: &mut [Element], node: Node) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    }
}

/// The tag of a heading at `level`.
fn heading(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    }
}

/// A link to `url`, if it's somewhere a link may go, the web or mail, or
/// else plain text, as a `span`.
fn link(url: &str) -> Element {
    let lower = url.trim_start().to_ascii_lowercase();
    let allowed = ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme));
    if allowed {
        el("a")
            .attr("href", url.trim())
            .attr("target", "_blank")
            .attr("rel", "noopener noreferrer")
    } else {
        el("span")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(nodes: &[Node]) -> String {
        use std::fmt::Write as _;

        fn write(node: &Node, out: &mut String) {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Element(element) => {
                    out.push('<');
                    out.push_str(element.tag);
                    for (name, value) in &element.attrs {
                        let _ = write!(out, " {name}=\"{value}\"");
                    }
                    out.push('>');
                    for child in &element.children {
                        write(child, out);
                    }
                    out.push_str("</");
                    out.push_str(element.tag);
                    out.push('>');
                }
            }
        }
        let mut out = String::new();
        for node in nodes {
            write(node, &mut out);
        }
        out
    }

    #[test]
    fn markdown_becomes_elements() {
        assert_eq!(
            html(&render(
                "# Title\n\nSome *words* and `code`.\n\n- one\n- two"
            )),
            "<h1>Title</h1><p>Some <em>words</em> and <code>code</code>.</p>\
             <ul><li>one</li><li>two</li></ul>"
        );
        assert_eq!(
            html(&render("```rust\nfn main() {}\n```")),
            "<pre data-language=\"rust\" class=\"code\">fn main() {}\n</pre>"
        );
        assert_eq!(
            html(&render("| a | b |\n|---|---|\n| 1 | 2 |")),
            "<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>"
        );
        // Columns lined up as the table says, as in the window.
        assert_eq!(
            html(&render("| a | b | c |\n|---|:-:|--:|\n| 1 | 2 | 3 |")),
            "<table><tr><th>a</th><th class=\"middle\">b</th><th class=\"right\">c</th></tr>\
             <tr><td>1</td><td class=\"middle\">2</td><td class=\"right\">3</td></tr></table>"
        );
    }

    #[test]
    fn html_and_scripts_stay_text() {
        let rendered = render("<script>alert(1)</script>\n\nhi <b onclick=x>there</b>");
        let text = html(&rendered);
        assert!(!rendered.iter().any(|node| matches!(node,
            Node::Element(element) if element.tag == "script" || element.tag == "b")));
        assert!(
            text.contains("<script>alert(1)</script>"),
            "as text: {text}"
        );
        let links = render("[a](javascript:alert(1)) [b](https://example.com) ![c](data:x)");
        let text = html(&links);
        assert!(!text.contains("javascript:"), "{text}");
        assert!(!text.contains("data:"), "{text}");
        assert!(text.contains("<a href=\"https://example.com\""), "{text}");
    }

    #[test]
    fn markdown_cut_off_while_streaming_still_closes() {
        assert_eq!(html(&render("```\nhalf")), "<pre class=\"code\">half</pre>");
        assert_eq!(html(&render("**bold")), "<p>**bold</p>");
    }
}
