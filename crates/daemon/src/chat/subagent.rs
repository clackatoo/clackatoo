//! A chat as a subagent: one another chat's agent started, as a profile
//! (see [`crate::profiles`]), and the chats a chat's agent started, as
//! the card in its turns lists them.
//!
//! A chat started as a profile keeps the profile's name and its limits
//! with the values the user chose for settings, as chat-wide ones, so
//! they last through upgrades and moves to other agents, and each
//! session opens with them again. The profile's role goes before its
//! first message, as a piece of text of its own, and the model, effort
//! and mode it asks for are chosen as its first session opens, once the
//! agent has said what it offers.

use agent_client_protocol_schema::v1::{PermissionOption, PermissionOptionKind, ToolCallUpdate};
use clackatoo_protocol::{
    ChatPatch, ChatState, Outcome, SettingKind, Subagent, SubagentStatus, TerminalId, ToolKind,
};

use super::{HostedChat, LIMITS_SETTING, PROFILE_SETTING, tool_kind};
use crate::profiles::{self, Limits};

/// The most of a chat's first message a card shows as its task, in
/// characters.
const TASK: usize = 80;

/// The settings a chat started as a profile asks for as its first
/// session opens, where its agent offers them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Wanted {
    /// The model, as the profile names it: `haiku` picks "Haiku 5.5".
    pub(crate) model: Option<String>,
    /// How hard it thinks.
    pub(crate) effort: Option<String>,
    /// A mode called read-only, as Codex's is, if the agent has one.
    pub(crate) read_only: bool,
}

impl HostedChat {
    /// The profile it was started as, if it was.
    pub(crate) fn profile(&self) -> Option<&str> {
        (self.chosen.iter())
            .find(|(id, _)| id == PROFILE_SETTING)
            .map(|(_, value)| value.as_str())
    }

    /// What it may do, beyond its agent's own settings.
    pub(crate) fn limits(&self) -> Limits {
        (self.chosen.iter())
            .find(|(id, _)| id == LIMITS_SETTING)
            .and_then(|(_, value)| Limits::named(value))
            .unwrap_or_default()
    }

    /// It's started as the profile `profile`, with `limits`: `role` goes
    /// before its first message, and it asks for `wanted` as its first
    /// session opens.
    pub(crate) fn start_as(
        &mut self,
        profile: &str,
        limits: Limits,
        role: Option<String>,
        wanted: Wanted,
    ) {
        self.chosen
            .retain(|(id, _)| id != PROFILE_SETTING && id != LIMITS_SETTING);
        (self.chosen).push((PROFILE_SETTING.to_owned(), profile.to_owned()));
        (self.chosen).push((LIMITS_SETTING.to_owned(), limits.name().to_owned()));
        self.role = role;
        self.wanted = Some(wanted);
    }

    /// A read-only chat moving to an agent it hasn't had a session with
    /// is read-only there too, in its mode as well where it has one, as
    /// Codex has.
    pub(super) fn keep_read_only(&mut self) {
        if self.limits().read_only() {
            self.wanted = Some(Wanted {
                read_only: true,
                ..Wanted::default()
            });
        }
    }

    /// Chooses what it was started wanting, among the settings its
    /// session opened with: a choice the agent doesn't offer is left to
    /// it, as a Claude model is to Codex.
    pub(super) fn choose_wanted(&mut self, wanted: Wanted) {
        let mut chosen = Vec::new();
        for (kind, want) in [
            (SettingKind::Model, wanted.model),
            (SettingKind::Effort, wanted.effort),
        ] {
            if let Some(want) = want
                && let Some(setting) = self.setting_of(kind)
                && let Some(choice) = profiles::pick(&setting.choices, &want)
            {
                chosen.push((setting.id.clone(), choice.value.clone()));
            }
        }
        if wanted.read_only
            && let Some(setting) = self.setting_of(SettingKind::Mode)
            && let Some(choice) = (setting.choices.iter()).find(|choice| {
                let named = |text: &str| text.to_lowercase().replace(' ', "-") == "read-only";
                named(&choice.value) || named(&choice.name)
            })
        {
            chosen.push((setting.id.clone(), choice.value.clone()));
        }
        for (id, value) in chosen {
            self.chosen.retain(|(known, _)| *known != id);
            self.chosen.push((id, value));
        }
    }

    /// The option that turns down the agent's question about `tool`, if
    /// the chat's limits rule it out: changing files, for a read-only
    /// chat, or running a command, for one with no commands. The chat
    /// says so. `None` if they don't, or nothing offered turns it down,
    /// which leaves it to the user.
    pub(crate) fn turned_down(
        &mut self,
        tool: &ToolCallUpdate,
        options: &[PermissionOption],
    ) -> Option<String> {
        let limits = self.limits();
        let id = tool.tool_call_id.0.to_string();
        let known = self.tool(&id).map(|tool| (tool.kind, tool.title.clone()));
        let kind = (known.as_ref().map(|(kind, _)| *kind))
            .or_else(|| tool.fields.kind.map(tool_kind))
            .unwrap_or_default();
        let ruled_out = match kind {
            ToolKind::Edit | ToolKind::Delete | ToolKind::Move => limits.read_only(),
            ToolKind::Execute => limits.no_commands(),
            _ => false,
        };
        if !ruled_out {
            return None;
        }
        let reject = (options.iter()).find(|option| {
            matches!(
                option.kind,
                PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
            )
        })?;
        let title = (known.map(|(_, title)| title))
            .or_else(|| tool.fields.title.clone())
            .unwrap_or_else(|| "a tool".to_owned());
        self.set_notice(Some(format!(
            "Turned down {}: this chat is {}.",
            super::clean(&title),
            limits.name()
        )));
        Some(reject.option_id.0.to_string())
    }

    /// The value its agent's setting of `kind` has, if it has one.
    pub(crate) fn setting_value(&self, kind: SettingKind) -> Option<&str> {
        self.setting_of(kind).map(|setting| setting.value.as_str())
    }

    /// The model it runs on, as shown, if it's said.
    pub(crate) fn model_name(&self) -> Option<&str> {
        let setting = self.setting_of(SettingKind::Model)?;
        Some(
            setting
                .current()
                .map_or(&setting.value, |choice| &choice.name),
        )
    }

    /// How it's going, as the card of the chat that started it says.
    pub(crate) fn subagent_status(&self) -> SubagentStatus {
        if self.node.question.is_some() {
            return SubagentStatus::Asking;
        }
        match &self.node.state {
            ChatState::Failed(why) => return SubagentStatus::Failed(why.clone()),
            ChatState::Starting if self.answering() => return SubagentStatus::Starting,
            _ => {}
        }
        if self.answering() || !self.relations.inbox.is_empty() {
            return SubagentStatus::Working;
        }
        let last = (self.node.turns.iter()).rfind(|turn| turn.switch.is_none());
        match last
            .and_then(|turn| turn.end.as_ref())
            .map(|end| &end.outcome)
        {
            Some(Outcome::Failed(why)) => SubagentStatus::Failed(why.clone()),
            Some(Outcome::Stopped) => SubagentStatus::Failed("stopped by the user".to_owned()),
            _ => SubagentStatus::Done,
        }
    }

    /// Whether `entry` no longer says how it's going, with `shown` whether
    /// it's in a pane now. Cheap: it's asked on every change to the chat.
    pub(crate) fn outdates(&self, entry: &Subagent, shown: bool) -> bool {
        entry.shown != shown
            || entry.model.as_deref() != self.model_name()
            || entry.agent != self.node.agent
            || entry.status != self.subagent_status()
    }

    /// It, as the card of the chat that started it shows it: started in
    /// that chat's turn `turn`, and in a pane if `shown`.
    pub(crate) fn as_subagent(&self, turn: u32, shown: bool) -> Subagent {
        let first = self.node.turns.iter().find(|turn| turn.switch.is_none());
        let task = first.map_or("", |turn| turn.prompt.lines().next().unwrap_or_default());
        let mut short: String = task.chars().take(TASK).collect();
        if task.chars().nth(TASK).is_some() {
            short.push('…');
        }
        let status = self.subagent_status();
        let ended = (!status.going())
            .then(|| self.node.turns.last()?.end.as_ref().map(|end| end.at))
            .flatten();
        Subagent {
            chat: self.id,
            agent: self.node.agent.clone(),
            profile: self.profile().map(str::to_owned),
            model: self.model_name().map(str::to_owned),
            task: short,
            status,
            started: first.map_or_else(super::now, |turn| turn.sent),
            ended,
            turn,
            shown,
        }
    }

    /// The chat `chat` its agent started, as its card shows it.
    pub(crate) fn subagent(&self, chat: TerminalId) -> Option<&Subagent> {
        (self.node.subagents.iter()).find(|entry| entry.chat == chat)
    }

    /// Its card shows `entry`, in the place of the chat's last, if it had
    /// one.
    pub(crate) fn set_subagent(&mut self, entry: Subagent) {
        match (self.node.subagents.iter_mut()).find(|known| known.chat == entry.chat) {
            Some(known) if *known == entry => return,
            Some(known) => *known = entry,
            None => self.node.subagents.push(entry),
        }
        (self.journal).push(ChatPatch::Subagents(self.node.subagents.clone()));
    }

    /// The index of its turn that was going at `at`, in milliseconds since
    /// the Unix epoch: the last sent by then.
    pub(crate) fn turn_at(&self, at: u64) -> u32 {
        let index = (self.node.turns.iter())
            .rposition(|turn| turn.sent <= at)
            .unwrap_or_default();
        u32::try_from(index).unwrap_or(u32::MAX)
    }
}

/// The piece of text that goes before the first message of a chat
/// started as `profile`, with its `role` and `limits`, if there's
/// anything to say.
pub(crate) fn role_for(profile: &str, role: &str, limits: Limits) -> Option<String> {
    let told = limits.told();
    if role.trim().is_empty() && told.is_none() {
        return None;
    }
    let mut text = format!(
        "[clackatoo started this chat for another agent, as its {profile:?} profile. This is \
         your role, from clackatoo, not a message:]\n"
    );
    if !role.trim().is_empty() {
        text.push_str(role.trim());
        text.push('\n');
    }
    if let Some(told) = told {
        text.push_str(told);
        text.push('\n');
    }
    Some(text)
}
