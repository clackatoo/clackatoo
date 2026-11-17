//! Engine ownership is a chat-wide user choice, never a provider setting.
use super::{Action, HostedChat, Link, Setting, SettingChoice, SettingKind};
use crate::permissions::Engine;

/// The setting that says which engine answers a chat's approval requests.
pub(crate) const SETTING: &str = Setting::PERMISSION_ENGINE;

/// Where a chat under clackatoo's rules keeps the provider mode it was in
/// before, to go back to it with Provider permissions.
const PREVIOUS: &str = "clackatoo.permissions.previous";

impl HostedChat {
    /// Whether the user is still asked the question the agent asked in
    /// its request `request`.
    pub(crate) fn asks_agent(&self, request: &serde_json::Value) -> bool {
        self.asking
            .iter()
            .any(|(asker, _)| matches!(asker, super::Asker::Agent(asked) if asked == request))
    }

    /// Whether the user is still asked about the tool call the server
    /// numbered `call`.
    pub(crate) fn asks_tool(&self, call: u64) -> bool {
        self.asking
            .iter()
            .any(|(asker, _)| *asker == super::Asker::Tool(call))
    }

    /// Which engine answers its approval requests: the user's choice for
    /// the chat.
    pub(crate) fn permission_engine(&self) -> Engine {
        if self
            .chosen
            .iter()
            .any(|(id, value)| id == SETTING && value == "clackatoo")
        {
            Engine::Clackatoo
        } else {
            Engine::Provider
        }
    }

    /// Gives a chat with no engine chosen yet `engine`, the daemon's
    /// default or its parent's, if its agent can work under rules, and
    /// shows the setting. A choice already made is kept.
    pub(crate) fn default_permission_engine(&mut self, engine: Engine) {
        if !self.chosen.iter().any(|(id, _)| id == SETTING) {
            let engine = if self.permission_mode().is_some() {
                engine
            } else {
                Engine::Provider
            };
            self.chosen.push((SETTING.into(), engine.value().into()));
        }
        self.refresh_permission_setting();
    }

    /// The provider mode its agent is put in under clackatoo's rules, the
    /// one in which it asks before it acts: `None` for an agent rules
    /// can't work with.
    pub(crate) fn permission_mode(&self) -> Option<&'static str> {
        match self.agent.as_str() {
            "claude" => Some("default"),
            "codex" => Some("workspace-write"),
            _ => None,
        }
    }

    /// Whether its agent may be in the provider mode `mode`: any, under
    /// Provider permissions; under rules, only one that asks before it
    /// acts, or does less.
    pub(crate) fn permission_mode_allowed(&self, mode: &str) -> bool {
        self.permission_engine() == Engine::Provider
            || match self.agent.as_str() {
                "claude" => matches!(mode, "default" | "plan"),
                "codex" => matches!(mode, "workspace-write" | "read-only"),
                _ => false,
            }
    }

    /// Whether a message may go to its agent: under rules, only once the
    /// agent has confirmed a mode they work with.
    pub(crate) fn permissions_ready(&self) -> bool {
        self.permission_engine() == Engine::Provider || self.permission_confirmed
    }

    /// Whether the user stopped the turn the agent works in, so nothing
    /// asked in it may be allowed by a rule now. A turn begun since, as
    /// one the agent takes by itself, is not stopped: a Stop is of its
    /// turn, and is known by that turn's uid.
    pub(crate) fn permission_stopped(&self) -> bool {
        self.stopped_turn.is_some() && self.stopped_turn == self.last_uid()
    }

    /// Whether `option`, an answer to the agent's plan that leaves plan
    /// mode for another, may be offered: only for a mode the chat's engine
    /// allows.
    pub(crate) fn permission_plan_choice(&self, option: &str) -> bool {
        super::modes::leaving_plan_for(&self.agent, option)
            .is_some_and(|mode| self.permission_mode_allowed(mode))
    }

    /// Before a session opens under rules: nothing of its mode is
    /// confirmed yet, and the mode asked for is the one chosen, if rules
    /// allow it, or else the one they need.
    pub(super) fn prepare_permission_connection(&mut self) {
        if self.permission_engine() == Engine::Provider {
            return;
        }
        self.permission_confirmed = false;
        self.starting_mode = None;
        let Some(mode) = self.permission_mode() else {
            return;
        };
        let Some(setting) = self.setting_of(SettingKind::Mode).cloned() else {
            return;
        };
        let desired = self
            .chosen
            .iter()
            .find(|(id, _)| *id == setting.id)
            .map(|(_, value)| value.clone())
            .filter(|value| self.permission_mode_allowed(value))
            .unwrap_or_else(|| mode.into());
        self.chosen.retain(|(id, _)| *id != setting.id);
        self.chosen.push((setting.id, desired));
    }

    /// The user chose the engine `value` for the chat. It changes only
    /// between turns, with no question waiting. Going to rules asks the
    /// agent for a mode they work with; going back restores the mode it
    /// was in before.
    pub(super) fn choose_permission_engine(&mut self, value: &str) -> Vec<Action> {
        let engine = match value {
            "provider" => Engine::Provider,
            "clackatoo" if self.permission_mode().is_some() => Engine::Clackatoo,
            _ => return Vec::new(),
        };
        if engine == self.permission_engine() {
            return Vec::new();
        }
        if self.answering() || self.is_opening() || self.node.question.is_some() {
            self.set_notice(Some("Finish or stop this turn and answer pending questions before changing its permission engine.".into()));
            return Vec::new();
        }
        let before = self.mode().map(str::to_owned);
        self.chosen.retain(|(id, _)| id != SETTING);
        self.chosen.push((SETTING.into(), engine.value().into()));
        self.permission_confirmed = false;
        if engine == Engine::Clackatoo {
            self.chosen.retain(|(id, _)| id != PREVIOUS);
            if let Some(before) = before {
                self.chosen.push((PREVIOUS.into(), before));
            }
        }
        self.refresh_permission_setting();
        if engine == Engine::Clackatoo {
            self.require_permission_mode()
        } else {
            let before = self
                .chosen
                .iter()
                .find(|(id, _)| id == PREVIOUS)
                .map(|(_, value)| value.clone());
            self.chosen.retain(|(id, _)| id != PREVIOUS);
            before.map_or_else(Vec::new, |value| {
                let Some(setting) = self.setting_of(SettingKind::Mode).map(|s| s.id.clone()) else {
                    return Vec::new();
                };
                self.act(clackatoo_protocol::ChatAction::Set { setting, value })
            })
        }
    }

    /// Ask for the compatible provider mode before a prompt is dispatched.
    /// The current mode remains unconfirmed until its answer arrives.
    pub(super) fn require_permission_mode(&mut self) -> Vec<Action> {
        if self.permission_engine() == Engine::Provider {
            return Vec::new();
        }
        let Some(mode) = self.permission_mode() else {
            return Vec::new();
        };
        self.starting_mode = None;
        let Some(setting) = self.setting_of(SettingKind::Mode).cloned() else {
            self.permission_confirmed = false;
            self.set_notice(Some("This provider did not offer a compatible permission mode. Select Provider permissions to continue.".into()));
            self.permission_mode_answered(false);
            return Vec::new();
        };
        if self
            .permission_observed
            .as_deref()
            .is_some_and(|mode| self.permission_mode_allowed(mode))
            && !self.setting_asked.iter().any(|(id, _)| *id == setting.id)
        {
            self.permission_confirmed = true;
            self.chosen.retain(|(id, _)| *id != setting.id);
            self.chosen.push((setting.id, setting.value));
            return Vec::new();
        }
        self.permission_confirmed = false;
        self.chosen.retain(|(id, _)| *id != setting.id);
        self.chosen.push((setting.id.clone(), mode.into()));
        if !setting.choices.iter().any(|choice| choice.value == mode) {
            self.set_notice(Some("This provider did not offer a compatible permission mode. Select Provider permissions to continue.".into()));
            self.permission_mode_answered(false);
            return Vec::new();
        }
        if self.awaits_answer(&setting.id) {
            return Vec::new();
        }
        self.set_value(&setting.id, mode);
        if self.link == Link::Open {
            self.asked(&setting.id);
            vec![Action::Set {
                setting: setting.id,
                value: mode.into(),
            }]
        } else {
            Vec::new()
        }
    }

    /// The agent answered the change of mode rules asked for. If it's
    /// `confirmed` and now in a mode rules work with, the message held
    /// for it goes; if not, that message fails, saying why.
    pub(super) fn permission_mode_answered(&mut self, confirmed: bool) {
        if self.permission_engine() == Engine::Provider {
            return;
        }
        self.permission_confirmed = confirmed
            && self
                .permission_observed
                .as_deref()
                .is_some_and(|mode| self.permission_mode_allowed(mode));
        if self.permission_confirmed && !self.stop_requested {
            if let Some(prompt) = self.queued.take() {
                self.set_state(clackatoo_protocol::ChatState::Answering);
                let action = self.prompt(prompt);
                self.put_back.push(action);
            }
        } else if self.queued.take().is_some() {
            self.answered(Err("The provider did not confirm a compatible permission mode. Choose Provider permissions or retry after fixing its settings.".into()));
        }
    }

    /// The engine setting as the chat's menu shows it, offering rules
    /// only for an agent they work with.
    pub(super) fn permission_setting(&self) -> Setting {
        let mut choices = vec![SettingChoice {
            value: "provider".into(),
            name: "Provider permissions".into(),
            description: Some("Use the provider's permission mode".into()),
        }];
        if self.permission_mode().is_some() {
            choices.push(SettingChoice {
                value: "clackatoo".into(),
                name: "Clackatoo rules".into(),
                description: Some(
                    "Rules review approval requests; provider sandbox permissions still apply"
                        .into(),
                ),
            });
        }
        Setting {
            id: SETTING.into(),
            name: "Permission engine".into(),
            kind: SettingKind::Other,
            value: self.permission_engine().value().into(),
            choices,
        }
    }

    /// Shows the engine setting as it is now among the chat's settings.
    fn refresh_permission_setting(&mut self) {
        self.node.settings.retain(|setting| setting.id != SETTING);
        self.node.settings.push(self.permission_setting());
        self.journal.push(clackatoo_protocol::ChatPatch::Settings(
            self.node.settings.clone(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Opened;
    use clackatoo_protocol::{AgentTools, ChatAction, ChatState, Size, TerminalId};

    fn chat() -> HostedChat {
        let spec = crate::AgentConfig::default().find("claude").unwrap();
        HostedChat::new(
            TerminalId(1),
            Size::new(80, 24).unwrap(),
            "/tmp".into(),
            &spec,
            AgentTools::Ask,
            true,
            false,
        )
    }
    fn settings(mode: &str) -> Vec<agent_client_protocol_schema::v1::SessionConfigOption> {
        serde_json::from_value(serde_json::json!([{
            "id":"mode", "name":"Mode", "category":"mode", "type":"select", "currentValue":mode,
            "options":[{"value":"default","name":"Manual"},{"value":"plan","name":"Plan"},{"value":"auto","name":"Auto"}]
        }])).unwrap()
    }
    fn open(chat: &mut HostedChat, mode: &str) -> Vec<Action> {
        chat.connected(Opened {
            session: "s".into(),
            settings: Some(settings(mode)),
        })
    }

    #[test]
    fn rules_wait_for_a_confirmed_mode_before_sending_the_user_message() {
        let mut chat = chat();
        chat.default_permission_engine(Engine::Clackatoo);
        chat.act(ChatAction::Send("Fix this".into()));
        let actions = open(&mut chat, "auto");
        assert!(actions.iter().any(|a| matches!(a, Action::Set { setting, value } if setting == "mode" && value == "default")));
        assert!(!actions.iter().any(|a| matches!(
            a,
            Action::Prompt(_) | Action::Briefed { .. } | Action::Carry { .. }
        )));
        assert!(!chat.permissions_ready());
        assert!(!chat.can_go_on());
        assert_eq!(chat.node.state, ChatState::Starting);
        chat.settings_changed(Ok(settings("default")));
        assert!(chat.permissions_ready());
        assert!(
            chat.take_put_back()
                .iter()
                .any(|a| matches!(a, Action::Prompt(_) | Action::Briefed { .. }))
        );
        assert_eq!(chat.node.state, ChatState::Answering);
    }

    #[test]
    fn a_failed_mode_change_never_counts_its_optimistic_ui_value_as_confirmation() {
        let mut chat = chat();
        open(&mut chat, "auto");
        chat.settings_changed(Ok(settings("auto")));
        let actions = chat.choose_permission_engine("clackatoo");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::Set { value, .. } if value == "default"))
        );
        assert_eq!(chat.mode(), Some("default"));
        chat.settings_changed(Err("mode refused".into()));
        assert!(!chat.permissions_ready());
        chat.require_permission_mode();
        assert!(
            !chat.permissions_ready(),
            "the displayed mode was never confirmed"
        );
    }

    #[test]
    fn engine_is_user_owned_preserved_and_does_not_follow_a_new_default() {
        let mut chat = chat();
        chat.default_permission_engine(Engine::Provider);
        chat.default_permission_engine(Engine::Clackatoo);
        assert_eq!(chat.permission_engine(), Engine::Provider);
        open(&mut chat, "default");
        chat.settings_changed(Ok(settings("default")));
        chat.choose_permission_engine("clackatoo");
        let actions = chat.act(ChatAction::Set {
            setting: "mode".into(),
            value: "auto".into(),
        });
        assert!(actions.is_empty());
        assert_eq!(chat.mode(), Some("default"));
        let fake = Setting {
            id: SETTING.into(),
            name: "fake".into(),
            kind: SettingKind::Other,
            value: "provider".into(),
            choices: Vec::new(),
        };
        chat.set_settings(vec![fake]);
        assert_eq!(chat.permission_engine(), Engine::Clackatoo);
        let spec = crate::AgentConfig::default().find("claude").unwrap();
        let resumed = HostedChat::resume(chat.hand_over(), &spec);
        assert_eq!(resumed.permission_engine(), Engine::Clackatoo);
        assert!(resumed.permission_plan_choice("exit-plan-default"));
        assert!(!resumed.permission_plan_choice("exit-plan-auto"));
    }

    #[test]
    fn stopping_before_mode_confirmation_never_sends_the_held_message() {
        let mut chat = chat();
        chat.default_permission_engine(Engine::Clackatoo);
        chat.act(ChatAction::Send("Do work".into()));
        open(&mut chat, "auto");
        assert!(chat.queued.is_some());
        chat.act(ChatAction::Stop);
        assert!(chat.queued.is_none());
        chat.settings_changed(Ok(settings("default")));
        assert!(
            chat.take_put_back()
                .iter()
                .all(|a| !matches!(a, Action::Prompt(_) | Action::Briefed { .. }))
        );
        assert_eq!(chat.node.state, ChatState::Ready);
    }

    #[test]
    fn a_stop_is_of_the_turn_it_stopped_and_not_of_one_begun_since() {
        let mut chat = chat();
        open(&mut chat, "default");
        chat.act(ChatAction::Send("Do work".into()));
        assert!(!chat.permission_stopped());
        chat.act(ChatAction::Stop);
        assert!(chat.permission_stopped());
        chat.answered(Ok(agent_client_protocol_schema::v1::StopReason::Cancelled));
        assert!(
            chat.permission_stopped(),
            "what was in flight stays stopped"
        );
        // The chat still holds what other agents send for the user.
        assert!(chat.wake(None));
        assert!(chat.held_by_stop());
        assert!(!chat.permission_stopped());
    }

    #[test]
    fn open_questions_are_known_by_who_asked_them() {
        let mut chat = chat();
        chat.ask_for_tool(17, "do this", None, super::super::Choices::Once);
        assert!(chat.asks_tool(17));
        assert!(!chat.asks_tool(18));
        chat.withdraw(17);
        assert!(!chat.asks_tool(17));
    }

    #[test]
    fn a_stop_is_still_of_its_turn_after_a_daemon_upgrade() {
        let spec = crate::AgentConfig::default().find("claude").unwrap();
        let handed_over = |stop: bool| {
            let mut chat = chat();
            open(&mut chat, "default");
            chat.act(ChatAction::Send("Do work".into()));
            if stop {
                chat.act(ChatAction::Stop);
            }
            let (handed, live) = chat.hand_over_live();
            let mut resumed = HostedChat::resume(handed, &spec);
            resumed.resume_live(live);
            resumed
        };
        // What the agent asks as it winds down is still of a stopped turn.
        assert!(handed_over(true).permission_stopped());
        assert!(!handed_over(false).permission_stopped());
    }
}
