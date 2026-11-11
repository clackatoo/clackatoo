//! Native specialist drafts. Nothing is persisted until the user presses Save.
use super::view::{ControlView, RowView};
use super::{Mode, Outcome, SettingsMenu};
use crate::options::easy::Page;
use crate::render::Ink;
use crate::vim::Stroke;
use clackatoo_protocol::{SpecialistDefinition, SpecialistFile, SpecialistScope};
use serde_json::{Value, json};

#[derive(Debug, Default)]
pub(super) struct Specialists {
    pub(super) entries: Vec<SpecialistDefinition>,
    sources: std::collections::BTreeMap<String, SpecialistFile>,
    pub(super) status: String,
    pub(super) draft: Option<Draft>,
    pub(super) saving: bool,
    ready: bool,
    pending: Option<SpecialistDefinition>,
}
#[derive(Debug)]
pub(super) struct Draft {
    original: SpecialistDefinition,
    fields: Vec<String>,
    editing: Option<Text>,
    creating: bool,
    scope: SpecialistScope,
    path: Option<String>,
    copying: bool,
}
#[derive(Debug)]
struct Text {
    field: usize,
    before: String,
    cursor: usize,
}

// Each field retains the provider-independent schema; map-valued model/effort
// defaults can be edited as JSON without losing another provider's defaults.
const FIELDS: [(&str, &str, &str); 20] = [
    (
        "Name",
        "name",
        "Lowercase letters, digits, hyphens and underscores. Names stay fixed after creation.",
    ),
    (
        "Description",
        "description",
        "A short description shown in the picker.",
    ),
    (
        "Instructions",
        "instructions",
        "The specialist's role and working instructions. Ctrl+J inserts a line; Enter finishes editing.",
    ),
    (
        "When to use",
        "when_to_use",
        "Helps agents decide when to delegate to this specialist.",
    ),
    (
        "Default provider",
        "agent",
        "Optional provider name, such as claude or codex. Blank inherits.",
    ),
    (
        "Default model",
        "model",
        "Blank uses the provider default; inherit keeps the parent model. A JSON object sets defaults by provider.",
    ),
    (
        "Default effort",
        "effort",
        "Optional effort name, or a JSON object of defaults by provider.",
    ),
    (
        "Access limits",
        "access",
        "none, no-commands, or read-only. More restrictive parent limits still apply.",
    ),
    (
        "Time limit (seconds)",
        "seconds",
        "Optional positive ceiling for each invocation.",
    ),
    (
        "Turn limit",
        "turns",
        "Optional positive ceiling. Requires the Claude adapter.",
    ),
    (
        "Cost limit (USD)",
        "cost_usd",
        "Optional positive USD ceiling. Requires the Claude adapter.",
    ),
    (
        "Skills",
        "skills",
        "Comma-separated names. Blank inherits; [] selects none.",
    ),
    (
        "Skillsets",
        "skillsets",
        "Comma-separated bundles, including other specialists. Blank inherits; [] selects none.",
    ),
    (
        "Can delegate to",
        "delegates_to",
        "Comma-separated specialist names. Blank allows all; [] allows none.",
    ),
    (
        "User invocation",
        "user_invocable",
        "Available in the specialist picker.",
    ),
    (
        "Agent invocation",
        "agent_invocable",
        "Available for agents to delegate to.",
    ),
    (
        "Save specialist",
        "save",
        "Validates and saves the complete draft for future chats.",
    ),
    (
        "Cancel",
        "cancel",
        "Discard this draft and return to the list.",
    ),
    (
        "Scope",
        "scope",
        "Personal applies across projects. Project saves .agents/specialists in this repository. Existing files keep their location.",
    ),
    (
        "MCP servers",
        "mcp_servers",
        "Comma-separated managed server names. Blank inherits; [] selects none. Skillsets may bundle server definitions.",
    ),
];

impl Specialists {
    pub(super) fn new() -> Self {
        Self {
            status: "Reading specialists…".into(),
            ..Self::default()
        }
    }
}

impl SettingsMenu {
    pub(crate) fn show_specialists(&mut self) {
        self.mode = Mode::Easy;
        self.picker.clear();
        self.typing = false;
        self.specialist_focus(0, Some(Page::Specialists));
    }
    pub(crate) fn specialist_files_came(&mut self, files: Vec<SpecialistFile>) {
        let entries = files.iter().map(|f| f.definition.clone()).collect();
        self.specialists.sources = files
            .into_iter()
            .map(|f| (f.definition.name.clone(), f))
            .collect();
        self.specialists_came(Ok(entries));
    }
    pub(super) fn specialist_field(&self, row: usize) -> usize {
        if self.specialists.draft.is_some() {
            const ORDER: &[usize] = &[
                18, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 19, 13, 14, 15, 16, 17,
            ];
            ORDER[row]
        } else {
            row
        }
    }
    pub(crate) fn specialists_came(&mut self, result: Result<Vec<SpecialistDefinition>, String>) {
        match result {
            Ok(mut entries) => {
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                self.specialists.status = format!(
                    "{} specialists. Customize one or create your own.",
                    entries.len()
                );
                self.specialists.entries = entries;
                self.specialists.ready = true;
            }
            Err(why) => {
                self.specialists.status = why;
                self.specialists.ready = false;
            }
        }
    }
    pub(crate) fn specialist_saved(
        &mut self,
        expected: &SpecialistDefinition,
        result: Result<(), String>,
    ) {
        if self.specialists.pending.as_ref() != Some(expected) {
            return;
        }
        self.specialists.pending = None;
        self.specialists.saving = false;
        match result {
            Ok(()) => {
                self.specialists.draft = None;
                self.said_saved = Some((
                    "Specialist saved. Future chats use the new definition.".into(),
                    false,
                ));
                self.specialist_focus(0, None);
            }
            Err(why) => self.said = Some((why, true)),
        }
    }
    pub(super) fn specialist_rows(&self) -> usize {
        if !self.specialists.ready {
            return 0;
        }
        if self.specialists.draft.is_some() {
            FIELDS.len()
        } else {
            self.specialists.entries.len() + usize::from(self.specialists.ready)
        }
    }
    pub(super) fn specialist_row(&self, at: usize) -> RowView {
        let row = |label: String, help: String, control| RowView {
            label,
            help,
            notes: Vec::new(),
            lock: None,
            control,
        };
        let Some(draft) = &self.specialists.draft else {
            if at == 0 {
                return row(
                    "Create specialist".into(),
                    "Give a reusable role its own instructions, defaults and limits.".into(),
                    ControlView::Button("Create"),
                );
            }
            let s = &self.specialists.entries[at - 1];
            let v: Value = serde_json::from_str(&s.settings).unwrap_or_default();
            let mut view = row(
                s.name.clone(),
                v["description"].as_str().unwrap_or_default().into(),
                ControlView::Button(
                    if self
                        .specialists
                        .sources
                        .get(&s.name)
                        .is_some_and(|f| !f.editable)
                    {
                        "Make a custom copy"
                    } else {
                        "Edit"
                    },
                ),
            );
            if let Some(place) = &s.locked {
                view.notes.push((
                    format!("Defined in {}:{}", place.file, place.line),
                    Ink::Muted,
                ));
            }
            if let Some(source) = self.specialists.sources.get(&s.name) {
                view.notes.push((
                    if source.path.is_empty() {
                        "Built in".into()
                    } else {
                        format!("{} · {}", scope_name(source.scope), source.path)
                    },
                    Ink::Muted,
                ));
            }
            return view;
        };
        let (label, _, help) = FIELDS[at];
        let text = &draft.fields[at];
        let editing = draft.editing.as_ref().is_some_and(|t| t.field == at);
        let control = match at {
            14 | 15 => ControlView::Toggle(text == "true"),
            18 => ControlView::Segmented {
                labels: vec!["Personal", "Project"],
                picked: Some(usize::from(draft.scope == SpecialistScope::Project)),
            },
            16 => ControlView::Button(if self.specialists.saving {
                "Saving…"
            } else {
                "Save"
            }),
            17 => ControlView::Button("Cancel"),
            2 | 3 => ControlView::Paragraph {
                text: display_text(draft, at),
                editing,
            },
            _ => ControlView::Field {
                text: if editing {
                    display_text(draft, at)
                } else {
                    text.clone()
                },
                editing: false,
            },
        };
        let mut view = row(label.into(), help.into(), control);
        if at == 0 {
            view.notes.push((
                draft.path.clone().unwrap_or_else(|| {
                    format!("New {} SPECIALIST.md file", scope_name(draft.scope))
                }),
                Ink::Muted,
            ));
        }
        if at == 0 && !draft.creating {
            view.notes.push((
                "To use a different name, create another specialist.".into(),
                Ink::Muted,
            ));
        }
        view
    }
    pub(super) fn specialist_scope(&mut self, project: bool) -> Outcome {
        if let Some(draft) = &mut self.specialists.draft
            && draft.creating
            && !self.specialists.saving
        {
            draft.scope = if project {
                SpecialistScope::Project
            } else {
                SpecialistScope::Personal
            };
            return Outcome::Changed;
        }
        Outcome::Nothing
    }
    pub(super) fn specialist_act(&mut self, at: usize) -> Outcome {
        if self.specialists.saving {
            return Outcome::Nothing;
        }
        if let Some(draft) = &mut self.specialists.draft {
            // Clicking another field finishes the current edit within the draft.
            draft.editing = None;
            match at {
                0 if !draft.creating => return Outcome::Nothing,
                18 if draft.creating => {
                    draft.scope = if draft.scope == SpecialistScope::Personal {
                        SpecialistScope::Project
                    } else {
                        SpecialistScope::Personal
                    }
                }
                18 => return Outcome::Nothing,
                14 | 15 => {
                    draft.fields[at] = if draft.fields[at] == "true" {
                        "false"
                    } else {
                        "true"
                    }
                    .into();
                }
                16 => match draft.definition() {
                    Ok(edit)
                        if draft.creating
                            && !draft.copying
                            && self.specialists.entries.iter().any(|s| s.name == edit.name) =>
                    {
                        self.said=Some(("That name is already used. Edit the existing specialist or choose another name.".into(),true));
                    }
                    Ok(edit) => {
                        self.specialists.pending = Some(edit.clone());
                        self.specialists.saving = true;
                        return Outcome::SaveSpecialist {
                            specialist: edit,
                            scope: draft.scope,
                            path: draft.path.clone(),
                        };
                    }
                    Err(why) => self.said = Some((why, true)),
                },
                17 => {
                    self.specialists.draft = None;
                    self.specialist_focus(0, None);
                }
                _ => {
                    draft.editing = Some(Text {
                        field: at,
                        before: draft.fields[at].clone(),
                        cursor: draft.fields[at].len(),
                    });
                }
            }
            return Outcome::Changed;
        }
        if !self.specialists.ready {
            return Outcome::Nothing;
        }
        let creating = at == 0;
        let original = if creating {
            SpecialistDefinition {
                name: String::new(),
                settings:
                    json!({"limits":{"access":"none"},"user_invocable":true,"agent_invocable":true})
                        .to_string(),
                saved: None,
                locked: None,
            }
        } else {
            self.specialists.entries[at - 1].clone()
        };
        let source = self.specialists.sources.get(&original.name);
        let copying = source.is_some_and(|f| !f.editable);
        let mut draft = Draft::new(original, creating || copying);
        if let Some(source) = source {
            draft.scope = source.scope;
            if source.editable {
                draft.path = Some(source.path.clone());
            } else {
                draft.original.saved = None;
            }
        }
        draft.copying = copying;
        self.specialists.draft = Some(draft);
        self.said = None;
        self.said_saved = None;
        self.specialist_focus(if creating { 1 } else { 3 }, None);
        Outcome::Changed
    }
    pub(super) fn finish_specialist_field(&mut self) {
        if let Some(draft) = &mut self.specialists.draft {
            draft.editing = None;
        }
    }
    pub(super) fn specialist_editing(&self) -> bool {
        self.specialist_page()
            && self
                .specialists
                .draft
                .as_ref()
                .is_some_and(|d| d.editing.is_some())
    }
    pub(super) fn specialist_paste(&mut self, text: &str) -> Outcome {
        let Some(draft) = &mut self.specialists.draft else {
            return Outcome::Nothing;
        };
        let Some(edit) = &mut draft.editing else {
            return Outcome::Nothing;
        };
        let text = if matches!(edit.field, 2 | 3) {
            text.replace('\r', "")
        } else {
            text.replace(['\r', '\n'], " ")
        };
        if draft.fields[edit.field].len() + text.len() > 128 * 1024 {
            return Outcome::Nothing;
        }
        draft.fields[edit.field].insert_str(edit.cursor, &text);
        edit.cursor += text.len();
        Outcome::Changed
    }
    pub(super) fn specialist_key(&mut self, stroke: Stroke) -> Outcome {
        let Some(draft) = &mut self.specialists.draft else {
            return Outcome::Nothing;
        };
        let Some(edit) = &mut draft.editing else {
            return Outcome::Nothing;
        };
        let text = &mut draft.fields[edit.field];
        let prev = |s: &str, c: usize| s[..c].char_indices().next_back().map_or(0, |(i, _)| i);
        let next = |s: &str, c: usize| s[c..].chars().next().map_or(c, |ch| c + ch.len_utf8());
        match stroke {
            Stroke::Escape => {
                text.clone_from(&edit.before);
                draft.editing = None;
            }
            Stroke::Enter | Stroke::Tab => {
                draft.editing = None;
            }
            Stroke::Left => edit.cursor = prev(text, edit.cursor),
            Stroke::Right => edit.cursor = next(text, edit.cursor),
            Stroke::Home => {
                edit.cursor = text[..edit.cursor].rfind('\n').map_or(0, |at| at + 1);
            }
            Stroke::End => {
                edit.cursor = text[edit.cursor..]
                    .find('\n')
                    .map_or(text.len(), |at| edit.cursor + at);
            }
            Stroke::Char {
                ch: 'a',
                ctrl: true,
            } => edit.cursor = 0,
            Stroke::Char {
                ch: 'e',
                ctrl: true,
            } => edit.cursor = text.len(),
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => {
                text.clear();
                edit.cursor = 0;
            }
            Stroke::Up | Stroke::Down => {
                edit.cursor = vertical_cursor(text, edit.cursor, stroke == Stroke::Up);
            }
            Stroke::Backspace => {
                let from = prev(text, edit.cursor);
                text.drain(from..edit.cursor);
                edit.cursor = from;
            }
            Stroke::Delete => {
                let to = next(text, edit.cursor);
                text.drain(edit.cursor..to);
            }
            Stroke::Char {
                ch: 'j',
                ctrl: true,
            } if matches!(edit.field, 2 | 3) => return self.specialist_paste("\n"),
            Stroke::Char { ch, ctrl: false } => return self.specialist_paste(&ch.to_string()),
            _ => {}
        }
        Outcome::Changed
    }
}
fn display_text(draft: &Draft, field: usize) -> String {
    let mut text = draft.fields[field].clone();
    if let Some(edit) = &draft.editing
        && edit.field == field
    {
        text.insert(edit.cursor, '│');
    }
    text
}
impl Draft {
    fn new(original: SpecialistDefinition, creating: bool) -> Self {
        let v: Value = serde_json::from_str(&original.settings).unwrap_or_default();
        let fields = FIELDS
            .iter()
            .enumerate()
            .map(|(at, (_, key, _))| {
                if at == 0 {
                    return original.name.clone();
                }
                let value = if (7..=10).contains(&at) {
                    &v["limits"][key]
                } else {
                    &v[key]
                };
                match value {
                    Value::Null => String::new(),
                    Value::String(s) => s.clone(),
                    Value::Array(a) => {
                        if a.is_empty() {
                            "[]".into()
                        } else {
                            a.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    }
                    v => v.to_string(),
                }
            })
            .collect();
        Self {
            original,
            fields,
            editing: None,
            creating,
            scope: SpecialistScope::Personal,
            path: None,
            copying: false,
        }
    }
    fn definition(&self) -> Result<SpecialistDefinition, String> {
        let name = self.fields[0].trim().to_owned();
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
        {
            return Err(
                "Name must contain 1–64 lowercase letters, digits, hyphens or underscores".into(),
            );
        }
        let mut v = json!({"limits":{}});
        if let Ok(original) = serde_json::from_str::<Value>(&self.original.settings)
            && let Some(includes) = original.get("includes")
        {
            v["includes"] = includes.clone();
        }
        for (at, (_, key, _)) in FIELDS.iter().enumerate().take(16).skip(1) {
            let text = &self.fields[at];
            let trimmed = text.trim();
            let value = match at {
                1..=3 => json!(text),
                5 | 6 if trimmed.starts_with('{') => {
                    let value: Value = serde_json::from_str(trimmed)
                        .map_err(|e| format!("{}: {e}", FIELDS[at].0))?;
                    if !value.is_object() {
                        return Err("Model and effort maps must be objects".into());
                    }
                    value
                }
                8..=10 if !trimmed.is_empty() => serde_json::from_str::<Value>(trimmed)
                    .ok()
                    .filter(Value::is_number)
                    .ok_or_else(|| format!("{} must be a positive number", FIELDS[at].0))?,
                11..=13 if !trimmed.is_empty() => {
                    if trimmed == "[]" {
                        json!([])
                    } else {
                        json!(
                            trimmed
                                .split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                        )
                    }
                }
                14 | 15 => json!(trimmed == "true"),
                _ if trimmed.is_empty() => continue,
                _ => json!(trimmed),
            };
            if (7..=10).contains(&at) {
                v["limits"][key] = value;
            } else {
                v[key] = value;
            }
        }
        let names = self.fields[19].trim();
        if !names.is_empty() {
            v["mcp_servers"] = if names == "[]" {
                json!([])
            } else {
                json!(
                    names
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                )
            };
        }
        Ok(SpecialistDefinition {
            name,
            settings: v.to_string(),
            saved: self.original.saved.clone(),
            locked: None,
        })
    }
}

pub(super) fn vertical_cursor(text: &str, cursor: usize, up: bool) -> usize {
    let start = text[..cursor].rfind('\n').map_or(0, |at| at + 1);
    let column = text[start..cursor].chars().count();
    let target = if up {
        if start == 0 {
            return 0;
        }
        let end = start - 1;
        let from = text[..end].rfind('\n').map_or(0, |at| at + 1);
        from..end
    } else {
        let Some(end) = text[cursor..].find('\n').map(|at| cursor + at) else {
            return text.len();
        };
        let from = end + 1;
        let end = text[from..].find('\n').map_or(text.len(), |at| from + at);
        from..end
    };
    target.start
        + text[target.clone()]
            .char_indices()
            .nth(column)
            .map_or(target.len(), |(at, _)| at)
}

fn scope_name(scope: SpecialistScope) -> &'static str {
    match scope {
        SpecialistScope::Personal => "Personal",
        SpecialistScope::Project => "Project",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_round_trips_every_optional_field_without_losing_provider_maps() {
        let settings = json!({"description":"Short","instructions":"One\nTwo","when_to_use":"Review",
            "agent":"claude","model":{"claude":"sonnet","codex":"gpt-6-luna"},"effort":{"claude":"high","codex":"low"},
            "limits":{"access":"read-only","seconds":30,"turns":4,"cost_usd":0.5},"skills":["check"],"skillsets":[],"delegates_to":["explore"],"user_invocable":true,"agent_invocable":false});
        let original = SpecialistDefinition {
            name: "reviewer".into(),
            settings: settings.to_string(),
            saved: Some("old".into()),
            locked: None,
        };
        let draft = Draft::new(original, false);
        let edit = draft.definition().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&edit.settings).unwrap(),
            settings
        );
        assert_eq!(edit.saved.as_deref(), Some("old"));
    }
    #[test]
    fn multiline_cursor_moves_between_unicode_lines_at_character_columns() {
        let text = "aéz\nñx\nlast";
        assert_eq!(vertical_cursor(text, 3, false), 8);
        assert_eq!(vertical_cursor(text, 8, true), 3);
        assert_eq!(vertical_cursor(text, 0, true), 0);
        assert_eq!(vertical_cursor(text, text.len(), false), text.len());
    }
}
