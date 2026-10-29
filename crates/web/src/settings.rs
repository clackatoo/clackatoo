//! View-only model navigation. Choices still use the daemon's setting IDs and values.
use crate::{
    action::Pick,
    ui::{Element, el},
};
use clackatoo_protocol::{ChatNode, ProviderModels, Setting, SettingKind};
use std::collections::BTreeSet;

#[derive(Debug, Default)]
pub struct Picker {
    pub query: String,
    pub provider: String,
    pub tab: Tab,
    pub favorites: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tab {
    #[default]
    Model,
    Permissions,
    Tools,
}

struct Model {
    name: String,
    provider: String,
    value: String,
    key: String,
    description: String,
    selected: bool,
}

fn models(node: &ChatNode) -> Vec<Model> {
    let mut rows = Vec::new();
    if let Some(setting) = node.settings.iter().find(|s| s.kind == SettingKind::Model) {
        for choice in &setting.choices {
            rows.push(Model {
                name: choice.name.clone(),
                provider: node.agent.clone(),
                value: Pick::set_value(&choice.value),
                key: format!("{}\u{1f}{}", node.agent.to_lowercase(), choice.value),
                description: choice.description.clone().unwrap_or_default(),
                selected: setting.value == choice.value,
            });
        }
    }
    for provider in &node.providers {
        if provider.title.eq_ignore_ascii_case(&node.agent)
            || provider.agent.eq_ignore_ascii_case(&node.agent)
        {
            continue;
        }
        if let ProviderModels::Known(choices) = &provider.models {
            for choice in choices {
                rows.push(Model {
                    name: choice.name.clone(),
                    provider: provider.title.clone(),
                    value: Pick::switch_value(&provider.agent, Some(&choice.value)),
                    key: format!("{}\u{1f}{}", provider.title.to_lowercase(), choice.value),
                    description: choice.description.clone().unwrap_or_default(),
                    selected: false,
                });
            }
        }
    }
    rows
}

pub fn panel(node: &ChatNode, picker: &Picker, capabilities: &[(String, String)]) -> Element {
    let mut panel = el("div").class("settings-panel").child(
        el("div")
            .class("settings-tabs")
            .attr("aria-label", "Agent settings")
            .children(
                [
                    ("Model", "model", Tab::Model),
                    ("Permissions", "permissions", Tab::Permissions),
                    ("Tools", "tools", Tab::Tools),
                ]
                .map(|(name, key, tab)| {
                    el("button")
                        .attr("data-picker-tab", key)
                        .class_if(picker.tab == tab, "selected")
                        .attr("aria-pressed", (picker.tab == tab).to_string())
                        .text(name)
                }),
            ),
    );
    if picker.tab == Tab::Tools {
        let mut content = el("div").class("usage-content");
        if capabilities.is_empty() {
            content = content.child(el("p").text("Reading live harness state…"));
        }
        for (name, detail) in capabilities {
            content = content.child(
                el("section")
                    .class("context-card")
                    .child(el("h3").text(name.clone()))
                    .child(el("p").text(detail.clone())),
            );
        }
        return panel.child(content);
    }
    if picker.tab == Tab::Permissions {
        return panel.children(
            node.settings
                .iter()
                .filter(|s| {
                    matches!(
                        s.kind,
                        SettingKind::Mode | SettingKind::Tools | SettingKind::Browser
                    ) || s.is_plan_mode()
                })
                .map(choices),
        );
    }
    panel = panel.child(
        el("label")
            .class("search-box model-search")
            .child(el("span").class("sr-only").text("Search all models"))
            .child(
                el("input")
                    .attr("id", "model-search")
                    .attr("type", "search")
                    .attr("placeholder", "Search all models…")
                    .attr("autocomplete", "off")
                    .attr("value", picker.query.clone()),
            ),
    );
    let mut tabs = el("div")
        .class("provider-tabs")
        .attr("aria-label", "Model providers");
    let mut providers = vec![node.agent.clone()];
    for provider in &node.providers {
        if !providers
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&provider.title))
        {
            providers.push(provider.title.clone());
        }
    }
    for provider in std::iter::once("Favorites".to_owned()).chain(providers) {
        let active = picker.query.is_empty() && picker.provider == provider;
        tabs = tabs.child(
            el("button")
                .attr("data-model-provider", provider.clone())
                .class_if(active, "selected")
                .attr("aria-pressed", active.to_string())
                .text(provider),
        );
    }
    panel = panel.child(tabs);
    panel.child(model_list(node, picker)).children(
        node.settings
            .iter()
            .filter(|s| matches!(s.kind, SettingKind::Effort | SettingKind::Other))
            .map(choices),
    )
}

fn model_list(node: &ChatNode, picker: &Picker) -> Element {
    let rows = models(node);
    let query = picker.query.trim().to_lowercase();
    let mut visible: Vec<_> = rows
        .iter()
        .filter(|m| {
            if !query.is_empty() {
                format!("{} {} {}", m.name, m.provider, m.description)
                    .to_lowercase()
                    .contains(&query)
            } else if picker.provider == "Favorites" {
                picker.favorites.contains(&m.key)
            } else {
                m.provider == picker.provider
                    || (picker.provider.is_empty() && m.provider == node.agent)
            }
        })
        .collect();
    if !query.is_empty() {
        visible.sort_by_key(|m| !picker.favorites.contains(&m.key));
    }
    let mut list = el("div").class("model-list").attr("aria-label", "Models");
    let setting = node
        .settings
        .iter()
        .find(|s| s.kind == SettingKind::Model)
        .map_or("model", |s| s.id.as_str());
    for model in visible {
        list = list.child(model_row(
            model,
            picker.favorites.contains(&model.key),
            setting,
        ));
    }
    if list.children.is_empty() {
        let message = if query.is_empty() && picker.provider == "Favorites" {
            "Star models to keep them here."
        } else if query.is_empty() {
            "No models available yet. Open this provider on the desktop to check its connection."
        } else {
            "No matching models. Try a model or provider name."
        };
        list = list.child(el("p").class("model-empty").text(message));
        if query.is_empty()
            && let Some(provider) = node.providers.iter().find(|p| p.title == picker.provider)
        {
            list = list.child(
                el("button")
                    .class("provider-default")
                    .attr("data-setting", setting)
                    .attr("data-pick", Pick::switch_value(&provider.agent, None))
                    .text(format!("Use {} default", provider.title)),
            );
        }
    }
    list
}

fn model_row(model: &Model, favorite: bool, setting: &str) -> Element {
    el("div")
        .class("model-row")
        .class_if(model.selected, "selected")
        .child(
            el("button")
                .class("model-pick")
                .attr("data-setting", setting)
                .attr("data-pick", model.value.clone())
                .attr("aria-pressed", model.selected.to_string())
                .child(
                    el("span")
                        .class("model-copy")
                        .child(el("strong").text(model.name.clone()))
                        .child(el("span").text(if model.description.is_empty() {
                            model.provider.clone()
                        } else {
                            format!("{} · {}", model.provider, model.description)
                        })),
                )
                .child(
                    el("span")
                        .class("selection-mark")
                        .attr("aria-hidden", "true")
                        .text(if model.selected { "✓" } else { "" }),
                ),
        )
        .child(
            el("button")
                .class("favorite")
                .class_if(favorite, "selected")
                .attr("data-favorite", model.key.clone())
                .attr(
                    "aria-label",
                    format!(
                        "{} {} favorite",
                        if favorite { "Remove" } else { "Add" },
                        model.name
                    ),
                )
                .attr("aria-pressed", favorite.to_string())
                .text(if favorite { "★" } else { "☆" }),
        )
}

fn choices(setting: &Setting) -> Element {
    let compact = matches!(setting.kind, SettingKind::Effort | SettingKind::Other)
        && setting.choices.iter().all(|c| c.description.is_none());
    el("fieldset")
        .class("setting-group")
        .class_if(compact, "compact")
        .child(el("legend").text(setting.name.clone()))
        .child(
            el("div")
                .class("setting-choices")
                .children(setting.choices.iter().map(|choice| {
                    let selected = setting.value == choice.value;
                    let mut button = el("button")
                        .class("setting-choice")
                        .class_if(selected, "selected")
                        .attr("data-setting", setting.id.clone())
                        .attr("data-pick", Pick::set_value(&choice.value))
                        .attr("aria-pressed", selected.to_string())
                        .child(el("strong").text(choice.name.clone()));
                    if let Some(description) = &choice.description {
                        button = button.child(el("span").text(description.clone()));
                    }
                    button
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clackatoo_protocol::{Provider, SettingChoice};
    fn node() -> ChatNode {
        let choices = |names: &[&str]| {
            names
                .iter()
                .map(|s| SettingChoice {
                    value: (*s).into(),
                    name: (*s).into(),
                    description: None,
                })
                .collect()
        };
        ChatNode {
            agent: "Codex".into(),
            settings: vec![
                Setting {
                    id: "model".into(),
                    name: "Model".into(),
                    kind: SettingKind::Model,
                    value: "Sol".into(),
                    choices: choices(&["Sol", "Luna"]),
                },
                Setting {
                    id: "mode".into(),
                    name: "Approvals".into(),
                    kind: SettingKind::Mode,
                    value: "ask".into(),
                    choices: choices(&["ask", "auto"]),
                },
            ],
            providers: vec![Provider {
                agent: "claude".into(),
                title: "Claude".into(),
                models: ProviderModels::Known(choices(&["Sonnet"])),
            }],
            ..ChatNode::default()
        }
    }
    fn picks(panel: &Element) -> Vec<&str> {
        panel
            .find_all("data-pick")
            .into_iter()
            .filter_map(|e| e.get("data-pick"))
            .collect()
    }
    #[test]
    fn searching_is_global_favorites_first_and_clearing_restores_provider() {
        let node = node();
        let mut picker = Picker {
            provider: "Codex".into(),
            query: "s".into(),
            ..Picker::default()
        };
        picker.favorites.insert("claude\u{1f}Sonnet".into());
        assert_eq!(
            picks(&panel(&node, &picker, &[])),
            ["switch:claude:Sonnet", "set:Sol"]
        );
        picker.query.clear();
        assert_eq!(picks(&panel(&node, &picker, &[])), ["set:Sol", "set:Luna"]);
        picker.provider = "Favorites".into();
        assert_eq!(picks(&panel(&node, &picker, &[])), ["switch:claude:Sonnet"]);
    }
    #[test]
    fn permissions_never_mix_in_model_choices() {
        let panel = panel(
            &node(),
            &Picker {
                tab: Tab::Permissions,
                ..Picker::default()
            },
            &[],
        );
        assert_eq!(picks(&panel), ["set:ask", "set:auto"]);
        assert!(panel.find_all("id").is_empty());
    }
}
