//! Native permission rule drafts; only a successful daemon save closes them.
use clackatoo_daemon::permissions::{Decision, Kind, Rule, read_rules};

use super::view::{ControlView, Part, RowView};
use super::{Facts, Mode, Outcome, Set, SettingsMenu};
use crate::options::{Value, easy::Page};
use crate::render::Ink;
use crate::vim::Stroke;

#[derive(Debug, Default)]
pub(super) struct Permissions {
    rules: Vec<Rule>,
    source: String,
    history: Vec<clackatoo_protocol::PermissionRecord>,
    draft: Option<Draft>,
    ready: bool,
    locked: bool,
    saving: bool,
}

#[derive(Debug)]
struct Draft {
    at: Option<usize>,
    decision: Decision,
    kind: Kind,
    target: String,
    arguments: String,
    editing: Option<(usize, String)>,
    base: String,
    deleting: bool,
}

impl SettingsMenu {
    pub(super) fn permission_page(&self) -> bool {
        self.mode == Mode::Easy && self.easy.page() == Page::Permissions && !self.searching()
    }
    pub(super) fn permission_drafting(&self) -> bool {
        self.permissions.draft.is_some()
    }

    pub(super) fn permission_follow(&mut self, facts: &Facts) {
        let info = super::find("permissions.rules").expect("declared permission rules");
        self.permissions.ready = facts.daemon.is_some_and(|daemon| daemon.knows(info));
        self.permissions.locked = facts.locked(info).is_some();
        if let Value::Text(text) = facts.get(info) {
            self.permissions.rules = read_rules(&text).unwrap_or_default();
            self.permissions.source = text;
        }
        self.permissions.history = facts
            .daemon
            .map(|d| d.permission_history.clone())
            .unwrap_or_default();
    }

    pub(crate) fn permission_saved(&mut self, saved: &clackatoo_protocol::OptionSaved) {
        if saved.name != "permissions.rules" || !self.permissions.saving {
            return;
        }
        self.permissions.saving = false;
        if let Some(why) = &saved.refused {
            self.said = Some((why.clone(), true));
        } else {
            self.permissions.draft = None;
            self.said_saved = Some((
                "Permission rules saved. Future approval requests use them.".into(),
                false,
            ));
            self.specialist_focus(1, Some(Page::Permissions));
        }
    }

    pub(super) fn permission_rows(&self) -> usize {
        if !self.permissions.ready {
            0
        } else if let Some(draft) = &self.permissions.draft {
            if draft.at.is_some() { 7 } else { 6 }
        } else {
            1 + self.permissions.rules.len()
        }
    }

    pub(super) fn permission_history_rows(&self) -> usize {
        1 + self.permissions.history.len()
    }

    pub(super) fn permission_history_row(&self, at: usize) -> RowView {
        if at == 0 {
            return row("Decision history".into(), "Latest 64 rule decisions; cleared on restart or upgrade. Refresh to read new decisions.".into(), ControlView::Button("Refresh"));
        }
        let record = &self.permissions.history[at - 1];
        let mut view = row(
            format!(
                "{} · chat {} · {}",
                record.decision, record.chat, record.tool
            ),
            record.reason.clone(),
            ControlView::Field {
                text: record.decision.clone(),
                editing: false,
            },
        );
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(record.at);
        view.notes
            .push((format!("{elapsed} seconds ago"), Ink::Muted));
        view
    }

    pub(super) fn permission_row(&self, at: usize) -> RowView {
        let Some(draft) = &self.permissions.draft else {
            let mut view = if at == 0 {
                row("Rules".into(), "Deny takes precedence over Ask, then Allow. Unmatched requests ask you. Tool and command matches are exact.".into(), ControlView::Button("Add rule"))
            } else {
                let rule = &self.permissions.rules[at - 1];
                row(
                    format!("Rule {} · {}", at, rule.decision.name()),
                    format!(
                        "{:?}: {}{}",
                        rule.kind,
                        rule.target,
                        if rule.arguments.is_some() {
                            " · exact arguments"
                        } else {
                            ""
                        }
                    ),
                    ControlView::Button("Edit"),
                )
            };
            if self.permissions.locked {
                view.notes.push((
                    "Rules are set in daemon/init.lua; edit that file to change them.".into(),
                    Ink::Muted,
                ));
            }
            return view;
        };
        let editing = |field| draft.editing.as_ref().is_some_and(|(at, _)| *at == field);
        let field = |text: &str, at| ControlView::Field {
            text: format!("{text}{}", if editing(at) { "│" } else { "" }),
            editing: editing(at),
        };
        match at {
            0 => row("Decision".into(), "Deny wins over Ask; Ask wins over Allow.".into(), ControlView::Segmented { labels: vec!["Allow", "Ask", "Deny"], picked: Some(match draft.decision { Decision::Allow => 0, Decision::Ask => 1, Decision::Deny => 2 }) }),
            1 => row("Match".into(), "Commands and tools match literally. Read and Edit match paths beneath a directory.".into(), ControlView::Segmented { labels: vec!["Tool", "Command", "Read", "Edit"], picked: Some(match draft.kind { Kind::Tool => 0, Kind::Command => 1, Kind::Read => 2, Kind::Edit => 3 }) }),
            2 => row("Target".into(), "Tool: clackatoo/list_panes or managed_docs/search. Command: git status. Directory: {project}, ~/src, or an absolute path. No wildcards.".into(), field(&draft.target, 2)),
            3 => row("Exact arguments".into(), "Optional complete JSON argument value for Tool rules. Blank matches any arguments. Never put credentials in rules.".into(), field(&draft.arguments, 3)),
            4 => row("Save rule".into(), "Save and validate the complete rule list.".into(), ControlView::Button(if self.permissions.saving { "Saving…" } else { "Save" })),
            5 => row("Cancel".into(), "Discard this draft.".into(), ControlView::Button("Cancel")),
            _ => row("Delete rule".into(), "Press twice to confirm deletion.".into(), ControlView::Button(if draft.deleting { "Confirm delete" } else { "Delete" })),
        }
    }

    pub(super) fn permission_act(
        &mut self,
        at: usize,
        part: Option<Part>,
        facts: &Facts,
    ) -> Outcome {
        self.permission_follow(facts);
        if self.permissions.saving || !self.permissions.ready {
            return Outcome::Nothing;
        }
        if self.permissions.locked || facts.automating {
            self.said = Some((
                "Permission rules are user-owned. Edit the configuration that sets them.".into(),
                true,
            ));
            return Outcome::Changed;
        }
        if self.permissions.draft.is_none() {
            let rule = at
                .checked_sub(1)
                .and_then(|at| self.permissions.rules.get(at));
            self.permissions.draft = Some(Draft {
                at: at.checked_sub(1),
                decision: rule.map_or(Decision::Ask, |r| r.decision),
                kind: rule.map_or(Kind::Tool, |r| r.kind),
                target: rule.map(|r| r.target.clone()).unwrap_or_default(),
                arguments: rule
                    .and_then(|r| r.arguments.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                editing: None,
                base: self.permissions.source.clone(),
                deleting: false,
            });
            self.specialist_focus(1, Some(Page::Permissions));
            return Outcome::Changed;
        }
        let draft = self.permissions.draft.as_mut().expect("draft exists");
        draft.editing = None;
        match at {
            0 => {
                let choices = [Decision::Allow, Decision::Ask, Decision::Deny];
                let current = choices
                    .iter()
                    .position(|d| *d == draft.decision)
                    .unwrap_or(1);
                let picked = if let Some(Part::Segment(at)) = part {
                    usize::from(at)
                } else {
                    (current + 1) % choices.len()
                };
                draft.decision = choices[picked.min(choices.len() - 1)];
            }
            1 => {
                let choices = [Kind::Tool, Kind::Command, Kind::Read, Kind::Edit];
                let current = choices.iter().position(|k| *k == draft.kind).unwrap_or(0);
                let picked = if let Some(Part::Segment(at)) = part {
                    usize::from(at)
                } else {
                    (current + 1) % choices.len()
                };
                draft.kind = choices[picked.min(choices.len() - 1)];
            }
            2 | 3 => {
                let before = if at == 2 {
                    &draft.target
                } else {
                    &draft.arguments
                };
                draft.editing = Some((at, before.clone()));
            }
            5 => {
                self.permissions.draft = None;
                self.specialist_focus(1, Some(Page::Permissions));
            }
            4 | 6 => {
                return self.permission_commit(at);
            }
            _ => {}
        }
        Outcome::Changed
    }

    fn permission_commit(&mut self, at: usize) -> Outcome {
        let draft = self.permissions.draft.as_mut().expect("draft exists");
        if at == 6 && !draft.deleting {
            draft.deleting = true;
            return Outcome::Changed;
        }
        if draft.base != self.permissions.source {
            self.said = Some((
                "Rules changed while this draft was open. Cancel and reopen it before saving."
                    .into(),
                true,
            ));
            return Outcome::Changed;
        }
        let mut rules = self.permissions.rules.clone();
        if at == 6 {
            if let Some(at) = draft.at {
                rules.remove(at);
            }
        } else {
            let arguments = if draft.arguments.trim().is_empty() {
                None
            } else {
                match serde_json::from_str(&draft.arguments) {
                    Ok(arguments) => Some(arguments),
                    Err(why) => {
                        self.said = Some((format!("Arguments must be JSON: {why}"), true));
                        return Outcome::Changed;
                    }
                }
            };
            let rule = Rule {
                decision: draft.decision,
                kind: draft.kind,
                target: draft.target.clone(),
                arguments,
            };
            if let Some(at) = draft.at {
                rules[at] = rule;
            } else {
                rules.push(rule);
            }
        }
        let text = serde_json::to_string(&rules).expect("rules serialize");
        if let Err(why) = read_rules(&text) {
            self.said = Some((why, true));
            return Outcome::Changed;
        }
        let info = super::find("permissions.rules").expect("declared permission rules");
        let value = Value::Text(text);
        self.permissions.saving = true;
        Outcome::Set(vec![Set {
            info,
            live: value.clone(),
            saved: Some(value),
        }])
    }

    pub(super) fn permission_editing(&self) -> bool {
        self.permission_page()
            && self
                .permissions
                .draft
                .as_ref()
                .is_some_and(|d| d.editing.is_some())
    }
    pub(super) fn permission_paste(&mut self, text: &str) -> Outcome {
        let Some(draft) = self
            .permissions
            .draft
            .as_mut()
            .filter(|_| !self.permissions.saving)
        else {
            return Outcome::Nothing;
        };
        let Some((field, _)) = draft.editing.as_ref() else {
            return Outcome::Nothing;
        };
        let target = if *field == 2 {
            &mut draft.target
        } else {
            &mut draft.arguments
        };
        let text = text.replace(['\r', '\n'], " ");
        if target.len() + text.len() <= 64 * 1024 {
            target.push_str(&text);
        }
        Outcome::Changed
    }
    pub(super) fn permission_key(&mut self, stroke: Stroke) -> Outcome {
        let Some(draft) = self
            .permissions
            .draft
            .as_mut()
            .filter(|_| !self.permissions.saving)
        else {
            return Outcome::Nothing;
        };
        let Some((field, before)) = &draft.editing else {
            return Outcome::Nothing;
        };
        let target = if *field == 2 {
            &mut draft.target
        } else {
            &mut draft.arguments
        };
        if *field == 2 && target.len() >= 4096 && matches!(stroke, Stroke::Char { ctrl: false, .. })
        {
            return Outcome::Nothing;
        }
        if target.len() >= 64 * 1024 && matches!(stroke, Stroke::Char { ctrl: false, .. }) {
            return Outcome::Nothing;
        }
        match crate::cmdline::simple::key(target, stroke) {
            crate::cmdline::simple::Outcome::Cancel => {
                target.clone_from(before);
                draft.editing = None;
            }
            crate::cmdline::simple::Outcome::Accept => draft.editing = None,
            _ if stroke == Stroke::Tab => draft.editing = None,
            _ => {}
        }
        Outcome::Changed
    }
}

fn row(label: String, help: String, control: ControlView) -> RowView {
    RowView {
        label,
        help,
        notes: Vec::new(),
        lock: None,
        control,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desk::Keys;
    use crate::options::{Case, Store};
    use crate::settings::DaemonFacts;
    use clackatoo_protocol::{DaemonOption, DaemonSettings, OptionSetBy, OptionSource};

    fn daemon() -> DaemonFacts {
        let options = clackatoo_daemon::settings::options::ALL
            .iter()
            .map(|info| DaemonOption {
                name: info.name.into(),
                value: clackatoo_daemon::settings::options::default_of(info).text(),
                running: None,
                restart: info.restart,
                by: OptionSetBy {
                    source: OptionSource::Default,
                    at: None,
                    inner: None,
                },
            })
            .collect();
        DaemonFacts::new(
            &DaemonSettings {
                machine: "test".into(),
                dir: "/tmp/config".into(),
                options,
                failure: None,
                permission_history: Vec::new(),
            },
            false,
        )
    }
    fn facts<'a>(store: &'a Store, keys: &'a Keys, daemon: &'a DaemonFacts) -> Facts<'a> {
        Facts {
            store,
            window: 1,
            keys,
            dir: None,
            automating: false,
            daemon: Some(daemon),
        }
    }

    #[test]
    fn native_draft_validates_and_waits_for_the_daemons_save_acknowledgement() {
        let (store, keys, daemon) = (Store::default(), Keys::default(), daemon());
        let facts = facts(&store, &keys, &daemon);
        let mut menu = SettingsMenu::new("", Case::default(), Mode::Easy, &facts, 0);
        menu.permission_act(0, None, &facts);
        menu.permission_act(0, Some(Part::Segment(0)), &facts);
        menu.permission_act(1, Some(Part::Segment(1)), &facts);
        menu.permission_act(2, None, &facts);
        menu.permission_paste("git status");
        menu.permission_key(Stroke::Enter);
        let Outcome::Set(sets) = menu.permission_act(4, None, &facts) else {
            panic!("draft did not save")
        };
        assert_eq!(sets[0].info.name, "permissions.rules");
        let Value::Text(text) = &sets[0].live else {
            panic!()
        };
        assert_eq!(read_rules(text).unwrap()[0].decision, Decision::Allow);
        assert!(menu.permissions.saving);
        assert!(menu.permissions.draft.is_some());
        menu.permission_saved(&clackatoo_protocol::OptionSaved {
            name: "permissions.rules".into(),
            value: Some(text.clone()),
            refused: Some("disk full".into()),
        });
        assert!(!menu.permissions.saving);
        assert!(
            menu.permissions.draft.is_some(),
            "failed saves retain the draft"
        );
        menu.permission_act(4, None, &facts);
        menu.permission_saved(&clackatoo_protocol::OptionSaved {
            name: "permissions.rules".into(),
            value: Some(text.clone()),
            refused: None,
        });
        assert!(menu.permissions.draft.is_none());
    }

    #[test]
    fn rule_edits_cannot_overwrite_concurrent_changes_or_be_saved_by_automated_input() {
        let (store, keys, mut daemon) = (Store::default(), Keys::default(), daemon());
        let mut menu = SettingsMenu::new(
            "",
            Case::default(),
            Mode::Easy,
            &facts(&store, &keys, &daemon),
            0,
        );
        menu.permission_act(0, None, &facts(&store, &keys, &daemon));
        menu.permissions.draft.as_mut().unwrap().target = "docs/search".into();
        let option = daemon
            .options
            .iter_mut()
            .find(|option| option.info.name == "permissions.rules")
            .unwrap();
        option.value =
            Value::Text(r#"[{"decision":"deny","kind":"tool","target":"docs/search"}]"#.into());
        let outcome = menu.permission_act(4, None, &facts(&store, &keys, &daemon));
        assert_eq!(outcome, Outcome::Changed);
        assert!(menu.said.as_ref().unwrap().0.contains("Rules changed"));
        let mut automated = facts(&store, &keys, &daemon);
        automated.automating = true;
        assert_eq!(menu.permission_act(4, None, &automated), Outcome::Changed);
        assert!(!menu.permissions.saving);
    }
}
