//! The desktop's provider marks, embedded locally and encoded only once.
use crate::ui::{Element, el};
use std::{fmt::Write as _, sync::OnceLock};

pub(crate) fn mark(agent: &str) -> Element {
    static CLAUDE: OnceLock<String> = OnceLock::new();
    static CODEX: OnceLock<String> = OnceLock::new();
    static GEMINI: OnceLock<String> = OnceLock::new();
    static ANTIGRAVITY: OnceLock<String> = OnceLock::new();
    let agent = agent.to_lowercase();
    let (cache, svg, color) = if agent.contains("claude") {
        (
            &CLAUDE,
            include_str!("../../gui/src/render/icons/claude.svg"),
            Some("#da7756"),
        )
    } else if agent.contains("antigravity") {
        (
            &ANTIGRAVITY,
            include_str!("../../gui/src/render/icons/antigravity.svg"),
            None,
        )
    } else if agent.contains("gemini") {
        (
            &GEMINI,
            include_str!("../../gui/src/render/icons/gemini.svg"),
            Some("#8eafff"),
        )
    } else if agent.contains("codex") || agent.contains("openai") {
        (
            &CODEX,
            include_str!("../../gui/src/render/icons/openai.svg"),
            Some("#a8a6a0"),
        )
    } else {
        return el("span")
            .class("provider-mark")
            .attr("aria-hidden", "true")
            .text("◇");
    };
    let source = cache.get_or_init(|| {
        let svg = color.map_or_else(
            || svg.to_owned(),
            |color| svg.replacen("<svg ", &format!("<svg fill=\"{color}\" "), 1),
        );
        let mut url = String::from("data:image/svg+xml,");
        for byte in svg.bytes() {
            let _ = write!(url, "%{byte:02X}");
        }
        url
    });
    el("img")
        .class("provider-mark")
        .attr("src", source.clone())
        .attr("alt", "")
}
