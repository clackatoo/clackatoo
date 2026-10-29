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

use agent_client_protocol_schema::v1::{
    PermissionOption, PermissionOptionKind, SessionConfigOption, ToolCallUpdate,
};
use clackatoo_protocol::{
    ChatPatch, ChatState, Outcome, SettingKind, Subagent, SubagentStatus, TerminalId, ToolKind,
};

use super::{HostedChat, KEPT_SETTING, LIMITS_SETTING, Message, PROFILE_SETTING, tool_kind};
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
    /// A mode called read-only, as Codex's is, if the agent has one: for
    /// a chat that may run no commands, as an agent whose commands run
    /// without asking in a sandbox, as Codex's, has no mode between.
    pub(crate) read_only: bool,
}

/// Where a chat an agent started stood, as the saved workspaces kept it:
/// what a daemon starting afresh makes it again with (see
/// `server::restore`), never more than it had.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Standing {
    /// The chat whose agent started it, made again, if it was.
    pub(crate) parent: Option<TerminalId>,
    /// The user's chat the chats starting it go back to, if it has a
    /// parent.
    pub(crate) root: Option<TerminalId>,
    /// How many chats started by agents lead down to it.
    pub(crate) depth: u8,
    /// The profile it was started as, if one.
    pub(crate) profile: Option<String>,
    /// What it may do beyond its agent's own settings.
    pub(crate) limits: Limits,
    /// The user had it in a pane and sent it back.
    pub(crate) kept: bool,
    /// The mode it was in, by its setting's ID and the value, if known.
    pub(crate) mode: Option<(String, String)>,
}

impl HostedChat {
    /// It's made again as the daemon starts afresh, as a chat an agent
    /// started that stood as `standing` says: its parent, profile and
    /// limits, together, and the mode it was in, asked for as its session
    /// opens, never the mode a new chat of the user's starts in.
    pub(crate) fn stand_again(&mut self, standing: Standing) {
        let Standing {
            parent,
            root,
            depth,
            profile,
            limits,
            kept,
            mode,
        } = standing;
        self.relations.parent = parent;
        self.relations.root = parent.and(root);
        self.relations.depth = depth;
        self.starting_mode = None;
        self.chosen
            .retain(|(id, _)| id != PROFILE_SETTING && id != LIMITS_SETTING);
        if let Some(profile) = profile {
            (self.chosen).push((PROFILE_SETTING.to_owned(), profile));
        }
        (self.chosen).push((LIMITS_SETTING.to_owned(), limits.name().to_owned()));
        if kept {
            self.keep_for_user();
        }
        if let Some((setting, value)) = mode {
            self.chosen.retain(|(id, _)| *id != setting);
            (self.chosen).push((setting.clone(), value.clone()));
            self.restored_mode = Some((setting, value));
        }
        // One that may run no commands opens in its agent's read-only
        // mode, where it has one, as it did as it started.
        if limits.no_commands() {
            self.wanted.get_or_insert_with(Wanted::default).read_only = true;
        }
    }

    /// It was made again as the daemon started afresh, and the chat that
    /// started it wasn't: it's on its own, and says so.
    pub(crate) fn stand_alone(&mut self) {
        self.set_notice(Some(
            "The chat that started this one wasn't kept through the restart, so it's on its own \
             now, with no more than it could do before."
                .to_owned(),
        ));
    }

    /// Whether an agent started it: it has a parent, or had one before a
    /// restart that didn't keep it. Such a chat never starts in a mode
    /// that may let it do more than it could.
    pub(crate) fn agents_chat(&self) -> bool {
        self.relations.parent.is_some() || self.relations.depth > 0
    }

    /// The mode it's in, by its setting's ID and the value, as the saved
    /// workspaces keep it: as its agent last said, or if its session
    /// hasn't opened since it was made again, the mode it was made again
    /// with.
    pub(crate) fn kept_mode(&self) -> Option<(String, String)> {
        match self.setting_of(SettingKind::Mode) {
            Some(setting) => Some((setting.id.clone(), setting.value.clone())),
            None => self.restored_mode.clone(),
        }
    }

    /// The profile it was started as, if it was.
    pub(crate) fn profile(&self) -> Option<&str> {
        (self.chosen.iter())
            .find(|(id, _)| id == PROFILE_SETTING)
            .map(|(_, value)| value.as_str())
    }

    /// Whether the user had it in a pane and sent it back to the
    /// background: it's theirs to close, never the agent's (`close_chat`).
    pub(crate) fn kept_by_user(&self) -> bool {
        (self.chosen.iter()).any(|(id, _)| id == KEPT_SETTING)
    }

    /// The user had it in a pane, and sent it back to the background.
    pub(crate) fn keep_for_user(&mut self) {
        if !self.kept_by_user() {
            (self.chosen).push((KEPT_SETTING.to_owned(), "yes".to_owned()));
        }
    }

    /// What it may do, beyond its agent's own settings.
    pub(crate) fn limits(&self) -> Limits {
        (self.chosen.iter())
            .find(|(id, _)| id == LIMITS_SETTING)
            .and_then(|(_, value)| Limits::named(value))
            .unwrap_or_default()
    }

    /// What its agent's mode rules out, as limits: a read-only mode, as
    /// Codex's, or a planning one, as Claude Code's, changes nothing. A
    /// chat it starts, which may have another agent and so not its mode,
    /// keeps these as limits of its own.
    pub(crate) fn mode_limits(&self) -> Limits {
        let Some(setting) = self.setting_of(SettingKind::Mode) else {
            return Limits::None;
        };
        let changes_nothing = |text: &str| {
            matches!(
                text.to_lowercase().replace([' ', '_'], "-").as_str(),
                "read-only" | "readonly" | "plan"
            )
        };
        let name = (setting.choices.iter())
            .find(|choice| choice.value == setting.value)
            .map_or("", |choice| choice.name.as_str());
        if changes_nothing(&setting.value) || changes_nothing(name) {
            Limits::ReadOnly
        } else {
            Limits::None
        }
    }

    /// Whether an agent offering `options` as a session's settings
    /// offers modes, one of which can be chosen.
    pub(crate) fn offers_modes(options: &[SessionConfigOption]) -> bool {
        (super::settings_of(options).iter())
            .any(|setting| setting.kind == SettingKind::Mode && !setting.choices.is_empty())
    }

    /// The mode it asks for as its session opens, by its value, if its
    /// session hasn't opened yet, or since it was made again as the daemon
    /// started, and it asks for one.
    pub(crate) fn mode_to_come(&self) -> Option<&str> {
        if self.session.is_none() {
            return self.starting_mode.as_deref();
        }
        // Made again as the daemon started, and not opened since: the
        // mode it was in, which it asks for again.
        if self.setting_of(SettingKind::Mode).is_none() {
            return (self.restored_mode.as_ref()).map(|(_, value)| value.as_str());
        }
        None
    }

    /// Where the mode `value` is among the modes its agent offers, from
    /// 0, the safest, if it offers that one.
    pub(crate) fn rank_of_mode(&self, value: &str) -> Option<usize> {
        let setting = self.setting_of(SettingKind::Mode)?;
        (setting.choices.iter()).position(|choice| choice.value == value)
    }

    /// Who sent the next message from another chat's agent, if it would be
    /// begun now, as [`Self::deliver_next`] begins it.
    pub(crate) fn next_message_from(&self) -> Option<TerminalId> {
        if self.answering() || self.user_queue.native.is_some() || self.user_queue.restart.is_some()
        {
            return None;
        }
        Some(self.relations.inbox.front()?.from.chat)
    }

    /// Drops the next message from another chat's agent, which may no
    /// longer be sent.
    pub(crate) fn drop_next_message(&mut self) {
        self.relations.inbox.pop_front();
    }

    /// Chats it started have finished: the notice that says so waits its
    /// turn, one only however many finish, whatever else waits.
    pub(crate) fn mark_finished(&mut self) {
        if !(self.relations.inbox.iter()).any(Message::is_finished_notice) {
            (self.relations.inbox).push_back(Message::finished_notice(self.id));
        }
    }

    /// Whether the next message to begin, now, is the notice that chats it
    /// started have finished, still to say what.
    pub(crate) fn finished_notice_next(&self) -> bool {
        self.next_message_from().is_some()
            && self
                .relations
                .inbox
                .front()
                .is_some_and(Message::is_unfilled_notice)
    }

    /// The notice that chats it started have finished, next, says `text`,
    /// or with none, goes: there's nothing left it hasn't heard.
    pub(crate) fn fill_finished_notice(&mut self, text: Option<String>) {
        match text {
            Some(text) => {
                if let Some(notice) = self.relations.inbox.front_mut() {
                    notice.text = text;
                }
            }
            None => drop(self.relations.inbox.pop_front()),
        }
    }

    /// Notices that chats finished have started `turns` turns under the
    /// user's chat `root` since they last wrote there, or here, as many as
    /// may be: the next waits for them, and the chat says so.
    pub(crate) fn notices_held(&mut self, turns: u32, root: TerminalId) {
        let place = if root == self.id {
            "here".to_owned()
        } else {
            format!("here or in chat {root}")
        };
        let said = format!(
            "What subagents found waits for you: their notices have started {turns} turns since \
             you last wrote. Write {place} to go on."
        );
        if self.node.notice.as_deref() != Some(said.as_str()) {
            self.set_notice(Some(said));
        }
    }

    /// Takes the notice that chats it started have finished out from in
    /// front of what else waits, as it waits for the user, so the next
    /// message behind it can be checked and begin: put it back with
    /// [`Self::put_back_notice`].
    pub(crate) fn take_held_notice(&mut self) -> Option<Message> {
        let front = self.relations.inbox.front()?;
        front
            .is_held_kind()
            .then(|| self.relations.inbox.pop_front())
            .flatten()
    }

    /// Puts back the notice [`Self::take_held_notice`] took, in front.
    pub(crate) fn put_back_notice(&mut self, notice: Message) {
        self.relations.inbox.push_front(notice);
    }

    /// Where its agent's mode is among the modes it offers, from 0, the
    /// safest, which comes first, as Manual or Read-only: later ones may
    /// do more. None if it offers no modes, or hasn't said yet.
    pub(crate) fn mode_rank(&self) -> Option<usize> {
        let setting = self.setting_of(SettingKind::Mode)?;
        (setting.choices.iter()).position(|choice| choice.value == setting.value)
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

    /// A chat that may run no commands, moving to an agent it hasn't had
    /// a session with, starts in that agent's read-only mode, where it has
    /// one, as Codex has.
    pub(super) fn keep_read_only(&mut self) {
        if self.limits().no_commands() {
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
        let mut missing = Vec::new();
        for (kind, want) in [
            (SettingKind::Model, wanted.model),
            (SettingKind::Effort, wanted.effort),
        ] {
            let Some(want) = want else { continue };
            let Some(setting) = self.setting_of(kind) else {
                continue;
            };
            match profiles::pick(&setting.choices, &want) {
                Some(choice) => chosen.push((setting.id.clone(), choice.value.clone())),
                None => missing.push(format!("{} {want:?}", setting.name.to_lowercase())),
            }
        }
        // Said, so the user sees why it's on the agent's own choice, which
        // may cost more than the profile meant.
        if !missing.is_empty() {
            self.set_notice(Some(format!(
                "{} offers no single {}, so it's on its own choice",
                self.node.agent,
                missing.join(" or ")
            )));
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
            || entry.tasks.len() != self.node.tasks.iter().filter(|task| task.going()).count()
            || entry
                .tasks
                .iter()
                .any(|task| !self.node.tasks.contains(task))
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
            tasks: self.tasks_going(),
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
