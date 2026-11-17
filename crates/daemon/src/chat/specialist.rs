//! Active specialist definitions travel with the chat, independently of
//! the provider session. Only their current role is returned after compaction.

use super::{
    Action, HostedChat, Link, SPECIALIST_SETTING, Setting, SettingChoice, SettingKind, Wanted,
    role_for,
};
use crate::specialists::{Budget, Specialist};

/// What the chat's specialist is kept as, among what it has chosen: its
/// definition, as JSON.
const DEFINITION: &str = "clackatoo.specialist.definition";

/// What the time its request must end by is kept as: milliseconds, as
/// [`super::now`] counts them.
const DEADLINE: &str = "clackatoo.specialist.deadline";

impl HostedChat {
    /// The specialist the chat was started as, as it was defined then.
    pub(crate) fn specialist_definition(&self) -> Option<&Specialist> {
        self.active_specialist.as_ref()
    }

    /// Keeps `definition` as the chat's specialist, through upgrades too.
    pub(crate) fn remember_specialist(&mut self, definition: Specialist) {
        let json = serde_json::to_string(&definition).expect("specialist serializes");
        self.chosen.retain(|(id, _)| id != DEFINITION);
        self.chosen.push((DEFINITION.into(), json));
        self.active_specialist = Some(definition);
    }

    /// Takes the chat's specialist back from what it has chosen, as a
    /// chat handed over by the build before is made again.
    pub(super) fn restore_specialist(&mut self) {
        for (id, _) in &mut self.chosen {
            if id == "clackatoo.profile" {
                *id = SPECIALIST_SETTING.into();
            }
        }
        let kept = self.chosen.iter().find(|(id, _)| id == DEFINITION);
        self.active_specialist = match kept.map(|(_, json)| serde_json::from_str(json)) {
            None => None,
            Some(Ok(definition)) => Some(definition),
            // Kept by a build whose definitions this one can't read. Its
            // limits on what the chat may do are kept apart, and hold.
            Some(Err(err)) => {
                log::warn!(
                    "chat {}: its specialist's definition can't be read: {err}",
                    self.id
                );
                self.notify(
                    "This chat's specialist couldn't be read back, so its choice of skills \
                     and MCP servers, who it may delegate to and its budgets are gone. Its \
                     access limits still hold. Start a new chat to use the specialist whole."
                        .to_owned(),
                );
                None
            }
        };
    }

    /// Offers `specialists`, those the user may choose, in the chat's
    /// settings.
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

    /// The setting the chat's specialist is chosen with.
    pub(super) fn specialist_setting(&self) -> Setting {
        Setting {
            id: SPECIALIST_SETTING.into(),
            name: "Specialist".into(),
            kind: SettingKind::Other,
            value: self.specialist().unwrap_or("general").into(),
            choices: self.specialist_choices.clone(),
        }
    }

    /// Starts the chat as `definition`, with `limits` on what it may do:
    /// only before its first message, as a role changed later would leave
    /// what was said under the old one. Its session is let go, and one
    /// opened as the specialist says. The server has checked the chat's
    /// agent can hold it to the specialist's budget.
    pub(crate) fn choose_specialist(
        &mut self,
        definition: Specialist,
        limits: crate::specialists::Limits,
    ) -> Result<Vec<Action>, String> {
        if self.answering() || !self.node.turns.is_empty() {
            return Err(
                "Choose a specialist before the first message. Open a new chat to use another \
                 specialist."
                    .into(),
            );
        }
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

    /// The ceilings the chat's specialist puts on each request.
    pub(crate) fn specialist_budget(&self) -> Budget {
        self.specialist_definition()
            .map_or(Budget::default(), |s| s.extra.budget)
    }

    /// Starts the time a request has, as one is sent, if the chat's
    /// specialist gives it a time.
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

    /// When the request being answered must end by, if it has a time.
    pub(crate) fn specialist_deadline(&self) -> Option<u64> {
        if !self.answering() {
            return None;
        }
        self.chosen
            .iter()
            .find(|(id, _)| id == DEADLINE)
            .and_then(|(_, s)| s.parse().ok())
    }

    /// The request's time is up: it's cancelled, and the chat says so.
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
    fn only_specialists_the_user_may_choose_are_in_the_picker() {
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
    }

    /// A definition a build to come kept, with a field this one doesn't
    /// know, is read all the same; one that can't be read at all is said
    /// in the chat, and its access limits, kept apart, still hold.
    #[test]
    fn a_kept_definition_is_read_leniently_and_one_that_cant_be_is_said() {
        let spec = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let definition = Specialist {
            name: "review".into(),
            extra: Extras {
                skills: Some(vec!["check".into()]),
                ..Extras::default()
            },
            ..Specialist::default()
        };
        // A chat started as it, as it's handed to the next build.
        let handed_over = || {
            let mut chat = chat();
            connect(&mut chat, "old");
            chat.choose_specialist(definition.clone(), Limits::ReadOnly)
                .unwrap();
            chat.hand_over()
        };
        let mut handed = handed_over();
        let kept = (handed.chosen.iter_mut())
            .find(|(id, _)| id == DEFINITION)
            .unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&kept.1).unwrap();
        json["later"] = serde_json::json!(true);
        json["extra"]["later"] = serde_json::json!([1]);
        json["extra"]["budget"]["later"] = serde_json::json!("x");
        kept.1 = json.to_string();
        let resumed = HostedChat::resume(handed, &spec);
        assert_eq!(resumed.specialist_definition(), Some(&definition));
        assert_eq!(resumed.node().notice, None);

        let mut handed = handed_over();
        let kept = (handed.chosen.iter_mut())
            .find(|(id, _)| id == DEFINITION)
            .unwrap();
        kept.1 = r#"{"name": 7}"#.to_owned();
        let resumed = HostedChat::resume(handed, &spec);
        assert_eq!(resumed.specialist_definition(), None);
        assert_eq!(resumed.limits(), Limits::ReadOnly);
        let notice = resumed.node().notice.clone().unwrap();
        assert!(notice.contains("couldn't be read back"), "{notice}");
    }
}
