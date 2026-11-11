//! Side-session state lives with chosen settings so live upgrades retain it.
use super::{HostedChat, LIMITS_SETTING, Wanted};
use crate::specialists::Limits;
use clackatoo_protocol::{AgentTools, SettingKind, TerminalId};
const PARENT: &str = "clackatoo.side-parent";
const SEED: &str = "clackatoo.side-seed";
const BASE_LIMIT: &str = "clackatoo.side-base-limit";

impl HostedChat {
    pub(crate) fn side_parent(&self) -> Option<TerminalId> {
        self.chosen
            .iter()
            .find(|(k, _)| k == PARENT)?
            .1
            .parse()
            .ok()
            .map(TerminalId)
    }
    pub(crate) fn make_side(
        &mut self,
        parent: TerminalId,
        seed: String,
        wanted: Wanted,
        limits: Limits,
    ) {
        self.chosen.push((PARENT.into(), parent.0.to_string()));
        self.chosen.push((BASE_LIMIT.into(), limits.name().into()));
        self.chosen
            .push((LIMITS_SETTING.into(), Limits::ReadOnly.name().into()));
        self.chosen.push((SEED.into(), seed.clone()));
        self.role = Some(seed);
        self.wanted = Some(wanted);
        self.set_tools(AgentTools::Off);
        self.set_browser(false);
        self.set_subagents(0);
    }
    pub(crate) fn side_wanted(&self) -> Wanted {
        Wanted {
            model: self.setting_value(SettingKind::Model).map(str::to_owned),
            effort: self.setting_value(SettingKind::Effort).map(str::to_owned),
            read_only: true,
        }
    }
    /// Remove side-only markers from a normal copy; retain the parent's limits.
    pub(crate) fn release_side(
        &mut self,
        tools: AgentTools,
        task: bool,
        subagents: u8,
        mode: Option<(String, String)>,
        cap: Limits,
    ) {
        let captured = self
            .chosen
            .iter()
            .find(|(k, _)| k == SEED)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let history = self
            .digest(0, self.node.turns.len(), &[])
            .unwrap_or_default();
        self.role = Some(format!(
            "{captured}\n\n{history}\n\nThis conversation is now a normal user chat. The temporary side-question restrictions above have ended. Follow the user's new instructions with the tools and permissions available in this session."
        ));
        let limits = self
            .chosen
            .iter()
            .find(|(k, _)| k == BASE_LIMIT)
            .map_or_else(|| "none".into(), |(_, v)| v.clone());
        self.chosen.retain(|(k, _)| {
            k != PARENT
                && k != BASE_LIMIT
                && k != SEED
                && k != LIMITS_SETTING
                && k != super::TASK_SETTING
                && k != super::SUBAGENTS_SETTING
        });
        let limits = Limits::named(&limits).unwrap_or(Limits::ReadOnly).max(cap);
        self.chosen
            .push((LIMITS_SETTING.into(), limits.name().into()));
        if let Some(mode) = mode {
            self.chosen.push(mode);
        }
        if !task {
            self.chosen.push((super::TASK_SETTING.into(), "off".into()));
        }
        self.set_subagents(subagents);
        self.set_tools(tools);
        self.wanted = Some(Wanted {
            read_only: self.limits().no_commands(),
            ..self.side_wanted()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn promotion_preserves_host_controls_and_the_stricter_access_limit() {
        let mut chat = super::super::tests::chat();
        chat.make_side(
            TerminalId(9),
            "Captured background".into(),
            Wanted::default(),
            Limits::NoCommands,
        );
        chat.release_side(
            AgentTools::Off,
            false,
            0,
            Some(("mode".into(), "plan".into())),
            Limits::ReadOnly,
        );
        assert!(chat.side_parent().is_none());
        assert_eq!(chat.limits(), Limits::ReadOnly);
        assert_eq!(chat.tools(), AgentTools::Off);
        assert!(!chat.task());
        assert_eq!(chat.subagents(), 0);
        assert!(chat.chosen.iter().any(|(k, v)| k == "mode" && v == "plan"));
    }
}
