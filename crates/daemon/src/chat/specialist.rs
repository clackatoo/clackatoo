//! Active specialist definitions travel with the chat, independently of
//! the provider session. Only their current role is returned after compaction.

use super::{
    Action, HostedChat, Link, SPECIALIST_SETTING, Setting, SettingChoice, SettingKind, Wanted,
    role_for,
};
use crate::specialists::{Budget, Specialist};

const DEFINITION: &str = "clackatoo.specialist.definition";
const DEADLINE: &str = "clackatoo.specialist.deadline";

impl HostedChat {
    pub(crate) fn specialist_definition(&self) -> Option<&Specialist> {
        self.active_specialist.as_ref()
    }

    pub(crate) fn remember_specialist(&mut self, definition: Specialist) {
        let json = serde_json::to_string(&definition).expect("specialist serializes");
        self.chosen.retain(|(id, _)| id != DEFINITION);
        self.chosen.push((DEFINITION.into(), json));
        self.active_specialist = Some(definition);
    }

    pub(super) fn restore_specialist(&mut self) {
        for (id, _) in &mut self.chosen {
            if id == "clackatoo.profile" {
                *id = SPECIALIST_SETTING.into();
            }
        }
        self.active_specialist = self
            .chosen
            .iter()
            .find(|(id, _)| id == DEFINITION)
            .and_then(|(_, json)| serde_json::from_str(json).ok());
    }

    pub(crate) fn offer_specialists(&mut self, specialists: &[Specialist]) {
        self.specialist_choices = specialists
            .iter()
            .filter(|s| s.extra.user_invocable)
            .map(|s| SettingChoice {
                value: s.name.clone(),
                name: s.name.clone(),
                description: (!s.description.is_empty()).then(|| s.description.clone()),
            })
            .collect();
        self.set_settings(self.agent_settings());
    }

    pub(super) fn specialist_setting(&self) -> Setting {
        Setting {
            id: SPECIALIST_SETTING.into(),
            name: "Specialist".into(),
            kind: SettingKind::Other,
            value: self.specialist().unwrap_or("general").into(),
            choices: self.specialist_choices.clone(),
        }
    }

    pub(crate) fn choose_specialist(
        &mut self,
        definition: Specialist,
        limits: crate::specialists::Limits,
    ) -> Result<Vec<Action>, String> {
        if self.answering() || !self.node.turns.is_empty() {
            return Err("Choose a specialist before the first message. Open a new chat to use another specialist.".into());
        }
        definition.extra.budget.check(self.agent())?;
        let wanted = Wanted {
            model: definition.model_for(self.agent()).map(str::to_owned),
            effort: definition.effort_for(self.agent()).map(str::to_owned),
            read_only: limits.no_commands(),
        };
        let role = role_for(&definition.name, &definition.instruction_text(), limits);
        self.start_as(&definition.name, limits, role, wanted);
        self.remember_specialist(definition);
        let mut actions = vec![Action::Abandon];
        if let Some(session) = self.session.take() {
            actions.push(Action::LetGo(session));
        }
        self.known = 0;
        self.link = Link::Opening;
        self.set_settings(self.agent_settings());
        actions.push(Action::Connect);
        Ok(actions)
    }

    pub(crate) fn specialist_budget(&self) -> Budget {
        self.specialist_definition()
            .map_or(Budget::default(), |s| s.extra.budget)
    }

    pub(crate) fn start_specialist_clock(&mut self) {
        self.chosen.retain(|(id, _)| id != DEADLINE);
        if let Some(seconds) = self.specialist_budget().seconds {
            self.chosen.push((
                DEADLINE.into(),
                super::now()
                    .saturating_add(seconds.saturating_mul(1000))
                    .to_string(),
            ));
        }
    }

    pub(crate) fn specialist_deadline(&self) -> Option<u64> {
        if !self.answering() {
            return None;
        }
        self.chosen
            .iter()
            .find(|(id, _)| id == DEADLINE)
            .and_then(|(_, s)| s.parse().ok())
    }

    pub(crate) fn specialist_timed_out(&mut self) -> Vec<Action> {
        self.chosen.retain(|(id, _)| id != DEADLINE);
        let mut actions = Vec::new();
        self.stop_for(&mut actions, true);
        self.notify(
            "Specialist time budget reached. The request was cancelled; its output is partial."
                .into(),
        );
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::tests::{chat, connect, send};
    use crate::specialists::{Extras, Limits};

    #[test]
    fn selected_role_limits_and_deadline_survive_handover() {
        let mut chat = chat();
        connect(&mut chat, "old");
        let definition = Specialist {
            name: "review".into(),
            instructions: "REVIEW INSTRUCTIONS".into(),
            extra: Extras {
                budget: Budget {
                    seconds: Some(30),
                    ..Budget::default()
                },
                ..Extras::default()
            },
            ..Specialist::default()
        };
        let actions = chat
            .choose_specialist(definition.clone(), Limits::ReadOnly)
            .unwrap();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::LetGo(s) if s == "old"))
        );
        assert_eq!(chat.specialist(), Some("review"));
        connect(&mut chat, "new");
        send(&mut chat, "check");
        let deadline = chat.specialist_deadline().unwrap();
        assert!(
            chat.choose_specialist(definition.clone(), Limits::None)
                .is_err()
        );
        let spec = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let continuation = chat.prepare_upgrade().expect("active request");
        let mut resumed = HostedChat::resume(chat.hand_over(), &spec);
        assert!(resumed.continue_after_upgrade(continuation, &[]));
        assert_eq!(resumed.specialist_definition(), Some(&definition));
        assert_eq!(resumed.limits(), Limits::ReadOnly);
        assert_eq!(resumed.specialist_deadline(), Some(deadline));
        connect(&mut resumed, "new");
        assert_eq!(resumed.specialist_deadline(), Some(deadline));
        let actions = resumed.specialist_timed_out();
        assert!(actions.iter().any(|a| matches!(a, Action::Cancel)));
        assert_eq!(resumed.specialist_deadline(), None);
        assert!(
            resumed
                .node()
                .notice
                .as_ref()
                .unwrap()
                .contains("output is partial")
        );
    }

    #[test]
    fn picker_visibility_and_unsupported_budgets_are_checked_before_selection() {
        let mut chat = chat();
        let hidden = Specialist {
            name: "helper".into(),
            extra: Extras {
                user_invocable: false,
                ..Extras::default()
            },
            ..Specialist::default()
        };
        chat.offer_specialists(&[
            Specialist {
                name: "visible".into(),
                ..Specialist::default()
            },
            hidden,
        ]);
        assert_eq!(chat.specialist_setting().choices.len(), 1);
        assert_eq!(chat.specialist_setting().choices[0].value, "visible");
        let spec = crate::agents::AgentConfig::default().find("codex").unwrap();
        let mut unsupported = HostedChat::new(
            clackatoo_protocol::TerminalId(2),
            clackatoo_protocol::Size::new(40, 10).unwrap(),
            "/".into(),
            &spec,
            clackatoo_protocol::AgentTools::Off,
            false,
            false,
        );
        let definition = Specialist {
            name: "bounded".into(),
            extra: Extras {
                budget: Budget {
                    turns: Some(3),
                    ..Budget::default()
                },
                ..Extras::default()
            },
            ..Specialist::default()
        };
        assert!(
            unsupported
                .choose_specialist(definition, Limits::None)
                .unwrap_err()
                .contains("cannot enforce")
        );
        assert_eq!(unsupported.specialist(), None);
    }
}
