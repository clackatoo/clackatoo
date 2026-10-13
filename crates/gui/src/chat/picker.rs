//! What the settings menu holds, row by row, before it's laid out: one
//! list that both drawing it and moving through it with the keyboard read.
//!
//! A model is picked by its family: Claude Code's adapter offers each of
//! Opus, Sonnet and the rest as an alias that always means its newest
//! version (`opus`, named "Opus 5.5"), beside versions pinned by their IDs
//! (`claude-opus-4-8`, "Opus 4.8"). Choices whose names start with the same
//! word, one of them an alias, are a family: a row that picks the alias,
//! with its pinned versions behind a button. Choices of agents that don't
//! name them so are rows of their own, as any other setting's are.
//!
//! Effort, whose choices go from least to most, is a slider.

use clackatoo_protocol::{Setting, SettingChoice, SettingKind};

use super::Menu;

/// Where the keyboard is in the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Spot {
    /// On a choice: its setting's ID and its value.
    Choice { setting: String, value: String },
    /// On a setting's slider, by its ID.
    Slider(String),
}

/// One row of the menu.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Row {
    /// A section's heading: its setting's name, and what's chosen, if the
    /// rows below don't show it.
    Heading {
        name: String,
        aside: Option<String>,
    },
    Choice(ChoiceRow),
    Slider(SliderRow),
}

/// A choice to click, or a model's family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChoiceRow {
    pub(super) setting: String,
    /// What clicking it sets the setting to.
    pub(super) value: String,
    pub(super) name: String,
    /// A family's version, after its name: the one chosen, or else the one
    /// it picks.
    pub(super) version: Option<String>,
    pub(super) description: Option<String>,
    /// It's chosen, or for a family, one of its versions is.
    pub(super) current: bool,
    /// The agent recommends it.
    pub(super) recommended: bool,
    /// It's one of a family's pinned versions, shown under it.
    pub(super) nested: bool,
    /// The family it heads, if it has pinned versions, and whether
    /// they're shown.
    pub(super) family: Option<(String, bool)>,
}

/// A setting whose choices go from least to most, as steps along a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SliderRow {
    pub(super) setting: String,
    /// Its steps, in order: their values and names.
    pub(super) steps: Vec<(String, String)>,
    /// The step chosen, unless the choice is the reset or something else.
    pub(super) at: Option<usize>,
    /// The choice that leaves it to the agent, off the line, if there is
    /// one: its value and name, and whether it's chosen.
    pub(super) reset: Option<(String, String, bool)>,
}

impl Row {
    /// Where the keyboard can be on it, if anywhere.
    pub(super) fn spot(&self) -> Option<Spot> {
        match self {
            Self::Heading { .. } => None,
            Self::Choice(row) => Some(Spot::Choice {
                setting: row.setting.clone(),
                value: row.value.clone(),
            }),
            Self::Slider(row) => Some(Spot::Slider(row.setting.clone())),
        }
    }
}

/// The value that leaves a slider's setting to the agent.
const RESET: &str = "default";

/// The rows of `menu`, for `settings`: each of its settings', under its
/// name when there are several.
pub(super) fn rows(settings: &[Setting], menu: &Menu) -> Vec<Row> {
    let shown: Vec<&Setting> = (menu.settings.iter())
        .filter_map(|id| settings.iter().find(|setting| setting.id == *id))
        .collect();
    let mut rows = Vec::new();
    for setting in &shown {
        let value = menu.value(setting);
        let slider = slider(setting, value);
        if shown.len() > 1 {
            // A slider's steps don't say which is chosen: its heading does.
            let aside = slider.as_ref().map(|_| {
                (setting.choices.iter())
                    .find(|choice| choice.value == value)
                    .map_or_else(|| value.to_owned(), |choice| choice.name.clone())
            });
            rows.push(Row::Heading {
                name: setting.name.clone(),
                aside,
            });
        }
        match slider {
            Some(slider) => rows.push(Row::Slider(slider)),
            None if setting.kind == SettingKind::Model => {
                family_rows(setting, menu.expanded.as_deref(), &mut rows);
            }
            None => rows.extend(setting.choices.iter().map(|choice| {
                Row::Choice(ChoiceRow {
                    current: choice.value == setting.value,
                    ..plain(setting, choice)
                })
            })),
        }
    }
    rows
}

/// `setting` as a slider on `value`, if it's an effort with steps to
/// slide along.
fn slider(setting: &Setting, value: &str) -> Option<SliderRow> {
    if setting.kind != SettingKind::Effort {
        return None;
    }
    let steps: Vec<(String, String)> = (setting.choices.iter())
        .filter(|choice| choice.value != RESET)
        .map(|choice| (choice.value.clone(), choice.name.clone()))
        .collect();
    if steps.len() < 2 {
        return None;
    }
    let reset = (setting.choices.iter())
        .find(|choice| choice.value == RESET)
        .map(|choice| (choice.value.clone(), choice.name.clone(), value == RESET));
    Some(SliderRow {
        setting: setting.id.clone(),
        at: steps.iter().position(|(step, _)| step == value),
        steps,
        reset,
    })
}

/// A model's choices, grouped into families, those of `expanded` with
/// their versions under them.
fn family_rows(setting: &Setting, expanded: Option<&str>, rows: &mut Vec<Row>) {
    let models = Models::of(setting);
    for entry in &models.entries {
        match entry {
            Entry::Plain(choice) => rows.push(Row::Choice(ChoiceRow {
                current: models.is_current(choice),
                recommended: models.is_recommended(choice),
                ..plain(setting, choice)
            })),
            Entry::Family {
                name,
                head,
                versions,
            } => {
                let chosen = std::iter::once(head)
                    .chain(versions)
                    .find(|choice| models.is_current(choice));
                let open = expanded == Some(name.as_str());
                rows.push(Row::Choice(ChoiceRow {
                    setting: setting.id.clone(),
                    value: head.value.clone(),
                    name: name.clone(),
                    version: version(name, chosen.unwrap_or(head)),
                    description: description(head),
                    current: chosen.is_some(),
                    recommended: std::iter::once(head)
                        .chain(versions)
                        .any(|choice| models.is_recommended(choice)),
                    nested: false,
                    family: Some((name.clone(), open)),
                }));
                if open {
                    rows.extend(versions.iter().map(|choice| {
                        Row::Choice(ChoiceRow {
                            description: None,
                            current: models.is_current(choice),
                            nested: true,
                            ..plain(setting, choice)
                        })
                    }));
                }
            }
        }
    }
}

/// The family of the version of a model chosen, if one's pinned: its
/// versions are shown when the menu opens.
pub(super) fn pinned_family(setting: &Setting) -> Option<String> {
    if setting.kind != SettingKind::Model {
        return None;
    }
    let models = Models::of(setting);
    models.entries.iter().find_map(|entry| match entry {
        Entry::Family { name, versions, .. }
            if versions.iter().any(|choice| models.is_current(choice)) =>
        {
            Some(name.clone())
        }
        _ => None,
    })
}

/// `choice` as a row of its own.
fn plain(setting: &Setting, choice: &SettingChoice) -> ChoiceRow {
    ChoiceRow {
        setting: setting.id.clone(),
        value: choice.value.clone(),
        name: choice.name.clone(),
        version: None,
        description: description(choice),
        current: false,
        recommended: false,
        nested: false,
        family: None,
    }
}

fn description(choice: &SettingChoice) -> Option<String> {
    (choice.description.clone()).filter(|description| !description.is_empty())
}

/// What `choice`'s name says after its family's: "5.5" for "Opus 5.5".
fn version(family: &str, choice: &SettingChoice) -> Option<String> {
    let rest = choice.name.strip_prefix(family)?.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// A model setting's choices, as families.
struct Models<'a> {
    setting: &'a Setting,
    entries: Vec<Entry<'a>>,
    /// The name of the choice the agent recommends, as its default
    /// choice's description gives it.
    recommended: Option<&'a str>,
}

enum Entry<'a> {
    Plain(&'a SettingChoice),
    /// Choices whose names start with `name`: the alias, and the rest.
    Family {
        name: String,
        head: &'a SettingChoice,
        versions: Vec<&'a SettingChoice>,
    },
}

impl<'a> Models<'a> {
    fn of(setting: &'a Setting) -> Self {
        // "Default (recommended)", described as "Opus 5.5": no row of its
        // own, but a mark on the one it means.
        let recommended = (setting.choices.iter())
            .find(|choice| choice.value == RESET)
            .and_then(|choice| choice.description.as_deref())
            .filter(|name| setting.choices.iter().any(|choice| choice.name == *name));
        let choices = (setting.choices.iter())
            .filter(|choice| recommended.is_none() || choice.value != RESET);
        let mut groups: Vec<(&str, Vec<&SettingChoice>)> = Vec::new();
        for choice in choices {
            let word = choice.name.split_whitespace().next().unwrap_or_default();
            match groups.iter_mut().find(|(name, _)| *name == word) {
                Some((_, members)) => members.push(choice),
                None => groups.push((word, vec![choice])),
            }
        }
        let mut entries = Vec::new();
        for (name, members) in groups {
            // Only an alias stands for the family: without one, there's no
            // telling which of them clicking the family should pick.
            let alias = members.iter().position(|choice| is_alias(choice));
            match alias {
                Some(alias) if members.len() > 1 => {
                    let mut versions = members;
                    let head = versions.remove(alias);
                    entries.push(Entry::Family {
                        name: name.to_owned(),
                        head,
                        versions,
                    });
                }
                _ => entries.extend(members.into_iter().map(Entry::Plain)),
            }
        }
        Self {
            setting,
            entries,
            recommended,
        }
    }

    /// Whether `choice` is the one chosen, or the agent's default, which
    /// it means, is.
    fn is_current(&self, choice: &SettingChoice) -> bool {
        choice.value == self.setting.value
            || (self.setting.value == RESET && self.is_recommended(choice))
    }

    fn is_recommended(&self, choice: &SettingChoice) -> bool {
        self.recommended == Some(choice.name.as_str())
    }
}

/// Whether `choice` is an alias, that means whichever version is newest:
/// `opus`, but not `claude-opus-4-8`.
fn is_alias(choice: &SettingChoice) -> bool {
    !choice.value.bytes().any(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(value: &str, name: &str, description: Option<&str>) -> SettingChoice {
        SettingChoice {
            value: value.into(),
            name: name.into(),
            description: description.map(str::to_owned),
        }
    }

    /// Claude Code's models, as its adapter offers them.
    fn models(value: &str) -> Setting {
        Setting {
            id: "model".into(),
            name: "Model".into(),
            kind: SettingKind::Model,
            value: value.into(),
            choices: vec![
                choice("default", "Default (recommended)", Some("Opus 5.5")),
                choice("opus", "Opus 5.5", Some("For complex work")),
                choice("sonnet", "Sonnet 5.5", Some("Most efficient")),
                choice("haiku", "Haiku 5.5", Some("Fastest")),
                choice("claude-haiku-4-5-20251001", "Haiku 4.5", Some("Fastest")),
                choice("claude-opus-5", "Opus 5", Some("Everyday")),
                choice("claude-opus-4-8", "Opus 4.8", Some("Everyday")),
            ],
        }
    }

    fn effort(value: &str) -> Setting {
        Setting {
            id: "effort".into(),
            name: "Effort".into(),
            kind: SettingKind::Effort,
            value: value.into(),
            choices: vec![
                choice("default", "Default", None),
                choice("low", "Low", None),
                choice("high", "High", None),
                choice("max", "Max", None),
            ],
        }
    }

    fn menu(settings: &[&str], expanded: Option<&str>) -> Menu {
        Menu {
            settings: settings.iter().map(|id| (*id).to_owned()).collect(),
            highlighted: None,
            expanded: expanded.map(str::to_owned),
            held: None,
        }
    }

    /// Each choice row's name and version, and whether it's chosen,
    /// recommended, nested, and has versions.
    fn shown(rows: &[Row]) -> Vec<(String, bool, bool, bool, bool)> {
        (rows.iter())
            .filter_map(|row| match row {
                Row::Choice(row) => Some((
                    match &row.version {
                        Some(version) => format!("{} {version}", row.name),
                        None => row.name.clone(),
                    },
                    row.current,
                    row.recommended,
                    row.nested,
                    row.family.is_some(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn models_are_picked_by_family_with_their_versions_behind_them() {
        let rows = rows(&[models("opus")], &menu(&["model"], None));
        let row =
            |name: &str, current, recommended| (name.to_owned(), current, recommended, false, true);
        assert_eq!(
            shown(&rows),
            [
                row("Opus 5.5", true, true),
                // Without pinned versions, the alias is a row of its own.
                ("Sonnet 5.5".to_owned(), false, false, false, false),
                row("Haiku 5.5", false, false),
            ]
        );
        // A family picks its alias, so it keeps up with new versions.
        let Row::Choice(opus) = &rows[0] else {
            panic!("{rows:?}")
        };
        assert_eq!((opus.name.as_str(), opus.value.as_str()), ("Opus", "opus"));
    }

    #[test]
    fn a_pinned_version_is_shown_under_its_family() {
        let setting = models("claude-opus-4-8");
        assert_eq!(pinned_family(&setting).as_deref(), Some("Opus"));
        let rows = rows(&[setting], &menu(&["model"], Some("Opus")));
        assert_eq!(
            shown(&rows)[..3],
            [
                ("Opus 4.8".to_owned(), true, true, false, true),
                ("Opus 5".to_owned(), false, false, true, false),
                ("Opus 4.8".to_owned(), true, false, true, false),
            ]
        );
        // The alias chosen: nothing's pinned.
        assert_eq!(pinned_family(&models("opus")), None);
    }

    #[test]
    fn the_default_model_is_the_one_it_recommends() {
        let rows = rows(&[models("default")], &menu(&["model"], None));
        assert!(shown(&rows)[0].1, "{rows:?}");
        assert_eq!(shown(&rows).len(), 3);
    }

    #[test]
    fn choices_named_otherwise_are_a_plain_list() {
        let mut setting = models("gpt-5");
        setting.choices = vec![
            choice("gpt-5", "GPT-5", None),
            choice("gpt-5-codex", "GPT-5 Codex", None),
            choice("o3", "o3", None),
        ];
        let rows = rows(&[setting], &menu(&["model"], None));
        let plain = |name: &str, current| (name.to_owned(), current, false, false, false);
        assert_eq!(
            shown(&rows),
            [
                plain("GPT-5", true),
                plain("GPT-5 Codex", false),
                plain("o3", false)
            ]
        );
    }

    #[test]
    fn effort_is_a_slider_with_the_default_off_it() {
        let rows = rows(
            &[models("opus"), effort("high")],
            &menu(&["model", "effort"], None),
        );
        let Some(Row::Slider(slider)) = rows.last() else {
            panic!("{rows:?}")
        };
        let values: Vec<&str> = slider
            .steps
            .iter()
            .map(|(value, _)| value.as_str())
            .collect();
        assert_eq!(values, ["low", "high", "max"]);
        assert_eq!(slider.at, Some(1));
        assert_eq!(
            slider.reset,
            Some(("default".into(), "Default".into(), false))
        );
        assert!(rows.contains(&Row::Heading {
            name: "Effort".into(),
            aside: Some("High".into())
        }));
        // On the default, no step is.
        let rows = super::rows(&[effort("default")], &menu(&["effort"], None));
        let Some(Row::Slider(slider)) = rows.last() else {
            panic!("{rows:?}")
        };
        assert_eq!(
            (slider.at, slider.reset.as_ref().map(|reset| reset.2)),
            (None, Some(true))
        );
    }

    #[test]
    fn an_effort_with_no_default_is_a_slider_with_nothing_off_it() {
        // As Codex's adapter offers it.
        let mut setting = effort("low");
        setting.choices.remove(0);
        let rows = rows(&[setting], &menu(&["effort"], None));
        let [Row::Slider(slider)] = rows.as_slice() else {
            panic!("{rows:?}")
        };
        assert_eq!((slider.at, slider.reset.as_ref()), (Some(0), None));
    }
}
