//! Chats with agents: the daemon's side of a chat pane.
//!
//! A chat has a terminal's ID and goes in panes as terminals do, but it
//! has no grid: it keeps the conversation as turns (a [`ChatNode`]), and
//! clients draw them. Every change to the turns goes into a journal of
//! [`ChatPatch`]es, which each viewer is sent from where it last was.
//!
//! The agent behind the chat is the server's: a chat asks for what it
//! needs with [`Action`]s, and is told what the agent said.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol_schema::MaybeUndefined;
use agent_client_protocol_schema::v1::{
    ContentBlock, PermissionOption, PermissionOptionKind, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOptions, SessionUpdate, StopReason,
    ToolCallStatus, ToolCallUpdate,
};
use clackatoo_protocol::{
    ChatAction, ChatNode, ChatPatch, ChatState, Choice, ChoiceKind, Item, Node, Outcome, Patch,
    Question, Setting, SettingChoice, SettingKind, Size, TerminalId, TerminalInfo, TerminalStatus,
    Tool, ToolStatus, Turn, TurnEnd, Usage, View, ViewUpdate,
};
use serde_json::Value;

use crate::agent::Opened;
use crate::handover;

/// The mode a new chat asks for, if the agent offers it: Claude Code's
/// Auto, which decides itself what needs asking about (jack, 2026-10-08).
const STARTING_MODE: &str = "auto";

/// How many changes a chat keeps for viewers that are behind. One further
/// behind gets the whole chat again.
const JOURNAL: usize = 4096;

/// What a chat asks the server to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    /// Send the agent this message.
    Prompt(String),
    /// Get the chat a session: its own again, or a new one.
    Connect,
    /// Stop the agent answering.
    Cancel,
    /// Answer the agent's question: with the option picked, or none.
    Answer {
        request: Value,
        option: Option<String>,
    },
    /// Change one of the agent's settings for the chat.
    Set { setting: String, value: String },
}

/// Whether a chat's session is open in the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Closed,
    /// The server has been asked to open it.
    Opening,
    Open,
}

/// A chat with an agent.
#[derive(Debug)]
pub(crate) struct HostedChat {
    id: TerminalId,
    /// The size of the pane it's in, for listing.
    size: Size,
    title: String,
    cwd: String,
    /// The agent's session, once it has made one.
    session: Option<String>,
    /// Whether the session is open in the agent now. It isn't before it's
    /// made, after an upgrade, or once the agent has gone.
    link: Link,
    /// A message sent while there's no session, to go once there is.
    queued: Option<String>,
    node: ChatNode,
    /// The JSON-RPC ID of the request the agent's question came in.
    asking: Option<Value>,
    /// The values the user gave settings, by their IDs, which the chat
    /// asks for again whenever its session opens: the agent forgets them
    /// when it stops.
    chosen: Vec<(String, String)>,
    /// The mode a new chat asks for when its session first opens.
    starting_mode: Option<&'static str>,
    journal: Journal,
}

/// The changes made to a chat, numbered, for viewers to catch up with.
#[derive(Debug, Default)]
struct Journal {
    /// The number of the first change kept.
    first: u64,
    patches: VecDeque<ChatPatch>,
}

impl Journal {
    fn push(&mut self, patch: ChatPatch) {
        if self.patches.len() == JOURNAL {
            self.patches.pop_front();
            self.first += 1;
        }
        self.patches.push_back(patch);
    }

    /// The number the next change will get.
    fn end(&self) -> u64 {
        self.first + self.patches.len() as u64
    }
}

/// What a viewer was last sent of a chat.
#[derive(Debug, Default)]
pub(crate) struct ChatSent {
    /// The number of the next change it hasn't had, once it has had the
    /// chat whole.
    next: Option<u64>,
    title: String,
}

impl HostedChat {
    /// A chat in `cwd`, not yet with a session: the server is to get it
    /// one.
    pub(crate) fn new(id: TerminalId, size: Size, cwd: String) -> Self {
        Self {
            id,
            size,
            title: "agent".to_owned(),
            cwd,
            session: None,
            link: Link::Opening,
            queued: None,
            node: ChatNode::default(),
            asking: None,
            chosen: Vec::new(),
            starting_mode: Some(STARTING_MODE),
            journal: Journal::default(),
        }
    }

    /// Hands the chat over to the next build. The agent doesn't go with
    /// it: the next build opens the session again when it's next needed,
    /// and an answer under way is cut off.
    pub(crate) fn hand_over(mut self) -> handover::Chat {
        if self.answering() {
            self.end_turn(Outcome::Failed("cut off by an upgrade".to_owned()));
        }
        let failure = match self.node.state {
            ChatState::Failed(why) => Some(why),
            _ => None,
        };
        handover::Chat {
            id: self.id.0,
            cols: self.size.cols(),
            rows: self.size.rows(),
            title: self.title,
            cwd: self.cwd,
            session: self.session,
            failure,
            turns: self.node.turns.into_iter().map(turn_to_handover).collect(),
            settings: (self.node.settings.into_iter())
                .map(setting_to_handover)
                .collect(),
            chosen: self.chosen,
        }
    }

    /// A chat the last build handed over.
    pub(crate) fn resume(chat: handover::Chat) -> Self {
        let size = Size::new(chat.cols, chat.rows).unwrap_or(Size::new(80, 24).expect("valid"));
        let mut resumed = Self::new(TerminalId(chat.id), size, chat.cwd);
        resumed.link = Link::Closed;
        resumed.title = chat.title;
        resumed.session = chat.session;
        resumed.node.turns = chat.turns.into_iter().map(turn_from_handover).collect();
        resumed.node.settings = (chat.settings.into_iter())
            .map(setting_from_handover)
            .collect();
        resumed.chosen = chat.chosen;
        resumed.starting_mode = None;
        resumed.node.state = chat.failure.map_or(ChatState::Ready, ChatState::Failed);
        resumed
    }

    pub(crate) fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            pid: None,
            size: self.size,
            title: self.title.clone(),
            status: TerminalStatus::Running,
        }
    }

    pub(crate) fn working_directory(&self) -> &str {
        &self.cwd
    }

    /// The agent's session, if it has made one.
    pub(crate) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub(crate) fn view(&self) -> View {
        View {
            title: self.title.clone(),
            root: Node::Chat(self.node.clone()),
        }
    }

    pub(crate) fn resize(&mut self, size: Size) {
        self.size = size;
    }

    /// What brings a viewer from what it was last sent to the chat now:
    /// the changes since, or the chat whole if it's had none or is too far
    /// behind. `None` if nothing changed.
    pub(crate) fn catch_up(&self, sent: &mut ChatSent) -> Option<ViewUpdate> {
        let end = self.journal.end();
        let Some(next) = sent.next.filter(|&next| next >= self.journal.first) else {
            sent.next = Some(end);
            self.title.clone_into(&mut sent.title);
            return Some(ViewUpdate::Snapshot(self.view()));
        };
        let mut patches = Vec::new();
        if self.title != sent.title {
            self.title.clone_into(&mut sent.title);
            patches.push(Patch::Title(self.title.clone()));
        }
        let skip = usize::try_from(next - self.journal.first).expect("the journal is short");
        for patch in self.journal.patches.iter().skip(skip) {
            // Pieces of the same words, streamed, go as one.
            if let (
                ChatPatch::Append { turn, item, text },
                Some(Patch::Chat(ChatPatch::Append {
                    turn: last_turn,
                    item: last_item,
                    text: last_text,
                })),
            ) = (patch, patches.last_mut())
                && (turn, item) == (last_turn, last_item)
            {
                last_text.push_str(text);
                continue;
            }
            patches.push(Patch::Chat(patch.clone()));
        }
        sent.next = Some(end);
        (!patches.is_empty()).then_some(ViewUpdate::Patch(patches))
    }

    // --- What the user asks -------------------------------------------------

    /// Takes what a client asks of the chat, and returns what the server
    /// must do about it.
    pub(crate) fn act(&mut self, action: ChatAction) -> Vec<Action> {
        let mut actions = Vec::new();
        match action {
            ChatAction::Send(text) => self.send(text, &mut actions),
            ChatAction::Stop => self.stop(&mut actions),
            ChatAction::Answer(option) => {
                if let Some(request) = self.asking.take() {
                    self.set_question(None);
                    actions.push(Action::Answer { request, option });
                }
            }
            // Only a value the agent offers is kept, and asked for.
            ChatAction::Set { setting, value } => {
                if self.set_value(&setting, &value) {
                    self.chosen.retain(|(id, _)| *id != setting);
                    self.chosen.push((setting.clone(), value.clone()));
                    if self.link == Link::Open {
                        actions.push(Action::Set { setting, value });
                    }
                }
            }
        }
        actions
    }

    /// Gives setting `id` the value `value`, as far as the chat's view
    /// goes. Returns whether it has that setting, with that choice.
    fn set_value(&mut self, id: &str, value: &str) -> bool {
        let Some(setting) = self
            .node
            .settings
            .iter_mut()
            .find(|setting| setting.id == id)
        else {
            return false;
        };
        if !setting.choices.iter().any(|choice| choice.value == value) {
            return false;
        }
        if setting.value != value {
            value.clone_into(&mut setting.value);
            self.journal
                .push(ChatPatch::Settings(self.node.settings.clone()));
        }
        true
    }

    /// Sends a message, if there is one and the agent isn't still
    /// answering the last.
    fn send(&mut self, text: String, actions: &mut Vec<Action>) {
        if text.trim().is_empty() || self.answering() {
            return;
        }
        self.set_notice(None);
        self.push_turn(Turn {
            prompt: text.clone(),
            sent: now(),
            items: Vec::new(),
            end: None,
        });
        match self.link {
            Link::Open => {
                self.set_state(ChatState::Answering);
                actions.push(Action::Prompt(text));
            }
            Link::Opening => self.queued = Some(text),
            Link::Closed => {
                self.queued = Some(text);
                self.link = Link::Opening;
                self.set_state(ChatState::Starting);
                actions.push(Action::Connect);
            }
        }
    }

    /// Stops the agent answering, or takes back the message if it hasn't
    /// gone yet.
    fn stop(&mut self, actions: &mut Vec<Action>) {
        if !self.answering() {
            return;
        }
        // A question waiting is answered with none, as the protocol asks.
        if let Some(request) = self.asking.take() {
            self.set_question(None);
            actions.push(Action::Answer {
                request,
                option: None,
            });
        }
        if self.link == Link::Open {
            actions.push(Action::Cancel);
        } else if self.queued.take().is_some() {
            self.end_turn(Outcome::Stopped);
        }
    }

    // --- What the agent says ------------------------------------------------

    /// The session asked for was made, or opened again. Returns the
    /// message waiting for it, to send.
    ///
    /// The settings the user chose go first, as the agent forgot them if
    /// it stopped, and a new chat asks for its starting mode.
    pub(crate) fn connected(&mut self, opened: Opened) -> Vec<Action> {
        self.link = Link::Open;
        // Opening one again answers with no ID: it's the one asked for.
        if !opened.session.is_empty() {
            self.session = Some(opened.session);
        }
        if let Some(options) = &opened.settings {
            self.set_settings(settings_of(options));
        }
        if let Some(mode) = self.starting_mode.take()
            && let Some(setting) = (self.node.settings.iter()).find(|setting| {
                setting.kind == SettingKind::Mode
                    && setting.choices.iter().any(|choice| choice.value == mode)
            })
            && !self.chosen.iter().any(|(id, _)| *id == setting.id)
        {
            self.chosen.push((setting.id.clone(), mode.to_owned()));
        }
        let mut actions = Vec::new();
        for (setting, value) in self.chosen.clone() {
            let differs = (self.node.settings.iter())
                .any(|known| known.id == setting && known.value != value);
            if differs && self.set_value(&setting, &value) {
                actions.push(Action::Set { setting, value });
            }
        }
        let queued = self.queued.take();
        self.set_state(if queued.is_some() {
            ChatState::Answering
        } else {
            ChatState::Ready
        });
        actions.extend(queued.map(Action::Prompt));
        actions
    }

    /// The agent's settings after the chat asked to change one, or why it
    /// couldn't.
    pub(crate) fn settings_changed(&mut self, result: Result<Vec<SessionConfigOption>, String>) {
        match result {
            Ok(options) if !options.is_empty() => self.set_settings(settings_of(&options)),
            Ok(_) => {}
            Err(err) => self.set_notice(Some(format!("The setting couldn't be changed: {err}"))),
        }
    }

    /// The session couldn't be made or opened.
    pub(crate) fn failed_to_connect(&mut self, reason: &str) {
        self.link = Link::Closed;
        if self.queued.take().is_some() {
            self.end_turn(Outcome::Failed(format!(
                "the agent couldn't start: {reason}"
            )));
        }
        self.set_state(ChatState::Failed(reason.to_owned()));
    }

    /// The agent has gone: it exited, or stopped answering.
    pub(crate) fn disconnected(&mut self) {
        let was_opening = self.link == Link::Opening;
        self.link = Link::Closed;
        self.queued = None;
        if self.answering() {
            self.end_turn(Outcome::Failed("the agent stopped".to_owned()));
            self.set_state(ChatState::Ready);
        } else if was_opening {
            self.set_state(ChatState::Failed("it stopped before it started".to_owned()));
        } else if self.session.is_some() {
            self.set_notice(Some(
                "The agent stopped. The next message starts it again.".to_owned(),
            ));
        }
    }

    /// The agent finished answering, or failed to.
    pub(crate) fn answered(&mut self, result: Result<StopReason, String>) {
        let outcome = match result {
            Ok(StopReason::Cancelled) => Outcome::Stopped,
            Ok(StopReason::MaxTokens) => {
                Outcome::Failed("the answer hit its length limit".to_owned())
            }
            Ok(StopReason::MaxTurnRequests) => {
                Outcome::Failed("the answer took too many steps".to_owned())
            }
            Ok(StopReason::Refusal) => Outcome::Failed("the agent refused".to_owned()),
            // Done, or a reason newer than this build knows.
            Ok(_) => Outcome::Done,
            Err(err) => Outcome::Failed(format!("the agent failed: {err}")),
        };
        if self.answering() {
            self.end_turn(outcome);
        }
        self.set_state(ChatState::Ready);
    }

    /// The agent asks whether it may use a tool.
    pub(crate) fn ask(
        &mut self,
        request: Value,
        tool: &ToolCallUpdate,
        options: &[PermissionOption],
    ) {
        let id = tool.tool_call_id.0.to_string();
        let title = tool
            .fields
            .title
            .clone()
            .or_else(|| self.tool(&id).map(|tool| tool.title.clone()))
            .unwrap_or_else(|| "use a tool".to_owned());
        let choices = options
            .iter()
            .map(|option| Choice {
                id: option.option_id.0.to_string(),
                name: option.name.clone(),
                kind: match option.kind {
                    PermissionOptionKind::AllowAlways => ChoiceKind::AllowAlways,
                    PermissionOptionKind::RejectOnce => ChoiceKind::RejectOnce,
                    PermissionOptionKind::RejectAlways => ChoiceKind::RejectAlways,
                    // Allowing once, or a kind newer than this build knows.
                    _ => ChoiceKind::AllowOnce,
                },
            })
            .collect();
        self.asking = Some(request);
        self.set_question(Some(Question {
            title: clean(&title),
            choices,
        }));
    }

    /// Takes a change to the session.
    pub(crate) fn update(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => self.write(&chunk.content, false),
            SessionUpdate::AgentThoughtChunk(chunk) => self.write(&chunk.content, true),
            SessionUpdate::ToolCall(call) => self.set_tool(Tool {
                id: call.tool_call_id.0.to_string(),
                title: clean(&call.title),
                status: tool_status(call.status),
            }),
            SessionUpdate::ToolCallUpdate(update) => {
                let id = update.tool_call_id.0.to_string();
                let mut tool = self.tool(&id).cloned().unwrap_or(Tool {
                    id,
                    title: "a tool".to_owned(),
                    status: ToolStatus::Running,
                });
                if let Some(title) = &update.fields.title {
                    tool.title = clean(title);
                }
                if let Some(status) = update.fields.status {
                    tool.status = tool_status(status);
                }
                self.set_tool(tool);
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                // Not a change to the turns: viewers pick it up anyway.
                if let MaybeUndefined::Value(title) = info.title {
                    self.title = clean(&title);
                }
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.set_settings(settings_of(&update.config_options));
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                let mode = update.current_mode_id.0.to_string();
                let id = (self.node.settings.iter())
                    .find(|setting| setting.kind == SettingKind::Mode)
                    .map(|setting| setting.id.clone());
                if let Some(id) = id {
                    self.set_value(&id, &mode);
                }
            }
            SessionUpdate::UsageUpdate(usage) => self.set_usage(Some(Usage {
                used: usage.used,
                size: usage.size,
            })),
            // Its plan and commands, which the chat doesn't show yet, and
            // the user's own words, already kept.
            _ => {}
        }
    }

    /// A piece of the agent's words, or of its thinking, on the end of the
    /// turn answering.
    fn write(&mut self, content: &ContentBlock, thinking: bool) {
        let ContentBlock::Text(text) = content else {
            return;
        };
        let Some(index) = self.answering_turn() else {
            return;
        };
        let text = clean_text(&text.text);
        let items = &mut self.node.turns[index].items;
        let continues = match items.last() {
            Some(Item::Words(_)) => !thinking,
            Some(Item::Thought(_)) => thinking,
            _ => false,
        };
        if !continues {
            // A new item goes whole, empty, with the text appended after.
            items.push(if thinking {
                Item::Thought(String::new())
            } else {
                Item::Words(String::new())
            });
            self.patch_turn(index);
        }
        let items = &mut self.node.turns[index].items;
        if let Some(Item::Words(words) | Item::Thought(words)) = items.last_mut() {
            words.push_str(&text);
        }
        let item = items.len() - 1;
        self.journal.push(ChatPatch::Append {
            turn: index_u32(index),
            item: index_u32(item),
            text,
        });
    }

    /// Adds `tool` to the turn answering, or changes the one with its ID.
    fn set_tool(&mut self, tool: Tool) {
        let Some(index) = self.answering_turn() else {
            return;
        };
        let items = &mut self.node.turns[index].items;
        let existing = items
            .iter_mut()
            .find(|item| matches!(item, Item::Tool(old) if old.id == tool.id));
        match existing {
            Some(item) => *item = Item::Tool(tool),
            None => items.push(Item::Tool(tool)),
        }
        self.patch_turn(index);
    }

    /// The tool with `id`, in the turn answering.
    fn tool(&self, id: &str) -> Option<&Tool> {
        let turn = self.node.turns.get(self.answering_turn()?)?;
        turn.items.iter().find_map(|item| match item {
            Item::Tool(tool) if tool.id == id => Some(tool),
            _ => None,
        })
    }

    // --- Changes ------------------------------------------------------------

    /// Whether a message has been sent and not yet answered.
    fn answering(&self) -> bool {
        self.answering_turn().is_some()
    }

    /// The turn being answered, if one is.
    fn answering_turn(&self) -> Option<usize> {
        let last = self.node.turns.len().checked_sub(1)?;
        self.node.turns[last].end.is_none().then_some(last)
    }

    /// Ends the turn being answered. Tools still running then never
    /// finished.
    fn end_turn(&mut self, outcome: Outcome) {
        let Some(index) = self.answering_turn() else {
            return;
        };
        let turn = &mut self.node.turns[index];
        for item in &mut turn.items {
            if let Item::Tool(tool) = item
                && tool.status == ToolStatus::Running
            {
                tool.status = ToolStatus::Failed;
            }
        }
        turn.end = Some(TurnEnd { at: now(), outcome });
        self.patch_turn(index);
        self.asking = None;
        self.set_question(None);
    }

    fn push_turn(&mut self, turn: Turn) {
        self.node.turns.push(turn);
        self.patch_turn(self.node.turns.len() - 1);
    }

    /// Records that turn `index` changed, sending it whole.
    fn patch_turn(&mut self, index: usize) {
        self.journal.push(ChatPatch::Turn {
            index: index_u32(index),
            turn: self.node.turns[index].clone(),
        });
    }

    fn set_state(&mut self, state: ChatState) {
        if self.node.state != state {
            self.node.state = state.clone();
            self.journal.push(ChatPatch::State(state));
        }
    }

    fn set_question(&mut self, question: Option<Question>) {
        if self.node.question != question {
            self.node.question.clone_from(&question);
            self.journal.push(ChatPatch::Question(question));
        }
    }

    fn set_settings(&mut self, settings: Vec<Setting>) {
        if self.node.settings != settings {
            self.node.settings.clone_from(&settings);
            self.journal.push(ChatPatch::Settings(settings));
        }
    }

    fn set_usage(&mut self, usage: Option<Usage>) {
        if self.node.usage != usage {
            self.node.usage = usage;
            self.journal.push(ChatPatch::Usage(usage));
        }
    }

    fn set_notice(&mut self, notice: Option<String>) {
        if self.node.notice != notice {
            self.node.notice.clone_from(&notice);
            self.journal.push(ChatPatch::Notice(notice));
        }
    }
}

/// Now, in milliseconds since the Unix epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The settings the agent offers, as the chat shows them. Only those that
/// pick from a list: on/off ones aren't shown yet.
fn settings_of(options: &[SessionConfigOption]) -> Vec<Setting> {
    options
        .iter()
        .filter_map(|option| {
            let SessionConfigKind::Select(select) = &option.kind else {
                return None;
            };
            let listed: Vec<_> = match &select.options {
                SessionConfigSelectOptions::Ungrouped(choices) => choices.iter().collect(),
                SessionConfigSelectOptions::Grouped(groups) => {
                    groups.iter().flat_map(|group| &group.options).collect()
                }
                // A shape newer than this build knows.
                _ => Vec::new(),
            };
            Some(Setting {
                id: option.id.0.to_string(),
                name: clean(&option.name),
                kind: match option.category {
                    Some(SessionConfigOptionCategory::Mode) => SettingKind::Mode,
                    Some(SessionConfigOptionCategory::Model) => SettingKind::Model,
                    Some(SessionConfigOptionCategory::ThoughtLevel) => SettingKind::Effort,
                    _ => SettingKind::Other,
                },
                value: select.current_value.0.to_string(),
                choices: listed
                    .into_iter()
                    .map(|choice| SettingChoice {
                        value: choice.value.0.to_string(),
                        name: clean(&choice.name),
                        description: choice.description.as_deref().map(clean),
                    })
                    .collect(),
            })
        })
        .collect()
}

fn index_u32(index: usize) -> u32 {
    u32::try_from(index).expect("far fewer than 4 billion turns or items")
}

fn tool_status(status: ToolCallStatus) -> ToolStatus {
    match status {
        ToolCallStatus::Completed => ToolStatus::Done,
        ToolCallStatus::Failed => ToolStatus::Failed,
        _ => ToolStatus::Running,
    }
}

/// A title or name on one line, without control characters.
fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .filter(|ch| !ch.is_control())
        .collect()
}

/// The agent's words without control characters, but with their line
/// breaks and tabs.
fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|&ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

fn setting_to_handover(setting: Setting) -> handover::ChatSetting {
    handover::ChatSetting {
        id: setting.id,
        name: setting.name,
        kind: match setting.kind {
            SettingKind::Mode => handover::SettingKind::Mode,
            SettingKind::Model => handover::SettingKind::Model,
            SettingKind::Effort => handover::SettingKind::Effort,
            SettingKind::Other => handover::SettingKind::Other,
        },
        value: setting.value,
        choices: (setting.choices.into_iter())
            .map(|choice| handover::ChatChoice {
                value: choice.value,
                name: choice.name,
                description: choice.description,
            })
            .collect(),
    }
}

fn setting_from_handover(setting: handover::ChatSetting) -> Setting {
    Setting {
        id: setting.id,
        name: setting.name,
        kind: match setting.kind {
            handover::SettingKind::Mode => SettingKind::Mode,
            handover::SettingKind::Model => SettingKind::Model,
            handover::SettingKind::Effort => SettingKind::Effort,
            handover::SettingKind::Other => SettingKind::Other,
        },
        value: setting.value,
        choices: (setting.choices.into_iter())
            .map(|choice| SettingChoice {
                value: choice.value,
                name: choice.name,
                description: choice.description,
            })
            .collect(),
    }
}

fn turn_to_handover(turn: Turn) -> handover::ChatTurn {
    handover::ChatTurn {
        prompt: turn.prompt,
        sent: turn.sent,
        items: (turn.items.into_iter())
            .map(|item| match item {
                Item::Words(words) => handover::ChatItem::Words(words),
                Item::Thought(words) => handover::ChatItem::Thought(words),
                Item::Tool(tool) => handover::ChatItem::Tool {
                    id: tool.id,
                    title: tool.title,
                    status: match tool.status {
                        ToolStatus::Running => handover::ToolStatus::Running,
                        ToolStatus::Done => handover::ToolStatus::Done,
                        ToolStatus::Failed => handover::ToolStatus::Failed,
                    },
                },
            })
            .collect(),
        end: turn.end.map(|end| handover::ChatEnd {
            at: end.at,
            outcome: match end.outcome {
                Outcome::Done => handover::ChatOutcome::Done,
                Outcome::Stopped => handover::ChatOutcome::Stopped,
                Outcome::Failed(why) => handover::ChatOutcome::Failed(why),
            },
        }),
    }
}

fn turn_from_handover(turn: handover::ChatTurn) -> Turn {
    Turn {
        prompt: turn.prompt,
        sent: turn.sent,
        items: (turn.items.into_iter())
            .map(|item| match item {
                handover::ChatItem::Words(words) => Item::Words(words),
                handover::ChatItem::Thought(words) => Item::Thought(words),
                handover::ChatItem::Tool { id, title, status } => Item::Tool(Tool {
                    id,
                    title,
                    status: match status {
                        handover::ToolStatus::Running => ToolStatus::Running,
                        handover::ToolStatus::Done => ToolStatus::Done,
                        handover::ToolStatus::Failed => ToolStatus::Failed,
                    },
                }),
            })
            .collect(),
        end: turn.end.map(|end| TurnEnd {
            at: end.at,
            outcome: match end.outcome {
                handover::ChatOutcome::Done => Outcome::Done,
                handover::ChatOutcome::Stopped => Outcome::Stopped,
                handover::ChatOutcome::Failed(why) => Outcome::Failed(why),
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol_schema::v1::SessionNotification;

    use super::*;

    fn chat() -> HostedChat {
        HostedChat::new(TerminalId(1), Size::new(40, 12).unwrap(), "/tmp".to_owned())
    }

    fn update(chat: &mut HostedChat, json: &str) {
        let json = format!(r#"{{"sessionId":"s","update":{json}}}"#);
        let notification: SessionNotification = serde_json::from_str(&json).unwrap();
        chat.update(notification.update);
    }

    fn words(chat: &mut HostedChat, text: &str) {
        let text = serde_json::to_string(text).unwrap();
        update(
            chat,
            &format!(
                r#"{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":{text}}}}}"#
            ),
        );
    }

    /// Opens `chat`'s session, `session`, with no settings.
    fn connect(chat: &mut HostedChat, session: &str) -> Vec<Action> {
        chat.connected(Opened {
            session: session.to_owned(),
            settings: None,
        })
    }

    /// Settings as Claude Code's adapter gives them: its mode, Manual, and
    /// its model, `model`.
    fn claude_settings(model: &str) -> Vec<SessionConfigOption> {
        serde_json::from_str(&format!(
            r#"[{{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"default",
                  "options":[{{"value":"default","name":"Manual"}},{{"value":"auto","name":"Auto"}}]}},
                {{"id":"model","name":"Model","category":"model","type":"select","currentValue":"{model}",
                  "options":[{{"value":"haiku","name":"Haiku"}},{{"value":"sonnet","name":"Sonnet"}}]}}]"#
        ))
        .unwrap()
    }

    fn send(chat: &mut HostedChat, text: &str) -> Vec<Action> {
        chat.act(ChatAction::Send(text.to_owned()))
    }

    fn outcome(turn: &Turn) -> Option<&Outcome> {
        turn.end.as_ref().map(|end| &end.outcome)
    }

    /// A viewer's copy of the chat, kept with the updates it's sent.
    struct Viewer {
        sent: ChatSent,
        view: Option<View>,
    }

    impl Viewer {
        fn new() -> Self {
            Self {
                sent: ChatSent::default(),
                view: None,
            }
        }

        /// Catches up, returning the update it got.
        fn catch_up(&mut self, chat: &HostedChat) -> Option<ViewUpdate> {
            let update = chat.catch_up(&mut self.sent)?;
            match (&mut self.view, update.clone()) {
                (Some(view), update) => view.apply(update),
                (None, ViewUpdate::Snapshot(view)) => self.view = Some(view),
                (None, ViewUpdate::Patch(_)) => panic!("a patch before a snapshot"),
            }
            Some(update)
        }
    }

    #[test]
    fn a_message_waits_for_the_session_then_goes() {
        let mut chat = chat();
        assert_eq!(chat.node.state, ChatState::Starting);
        // Sent before the session is made: it waits.
        assert_eq!(send(&mut chat, "hello"), []);
        assert_eq!(chat.node.turns[0].prompt, "hello");
        assert_eq!(connect(&mut chat, "s1"), [Action::Prompt("hello".into())]);
        assert_eq!(
            (chat.session(), &chat.node.state),
            (Some("s1"), &ChatState::Answering)
        );
        chat.answered(Ok(StopReason::EndTurn));
        // Sent once there's a session: it goes at once.
        assert_eq!(send(&mut chat, "again"), [Action::Prompt("again".into())]);
        // And not while it's being answered.
        assert_eq!(send(&mut chat, "more"), []);
    }

    #[test]
    fn words_stream_into_the_turn_and_tools_go_between_them() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "Let me ");
        words(&mut chat, "look.\n");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Read a.rs","status":"in_progress"}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}"#,
        );
        words(&mut chat, "Done.");
        chat.answered(Ok(StopReason::EndTurn));
        let turn = &chat.node.turns[0];
        assert_eq!(
            turn.items,
            [
                Item::Words("Let me look.\n".into()),
                Item::Tool(Tool {
                    id: "t1".into(),
                    title: "Read a.rs".into(),
                    status: ToolStatus::Done,
                }),
                Item::Words("Done.".into()),
            ]
        );
        assert_eq!(outcome(turn), Some(&Outcome::Done));
    }

    #[test]
    fn viewers_get_the_chat_then_its_changes_with_streamed_words_as_one() {
        let mut chat = chat();
        let mut viewer = Viewer::new();
        assert!(matches!(
            viewer.catch_up(&chat),
            Some(ViewUpdate::Snapshot(_))
        ));
        assert_eq!(viewer.catch_up(&chat), None);
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "Hel");
        words(&mut chat, "lo");
        words(&mut chat, " there");
        let Some(ViewUpdate::Patch(patches)) = viewer.catch_up(&chat) else {
            panic!("patches");
        };
        let appends: Vec<&Patch> = (patches.iter())
            .filter(|patch| matches!(patch, Patch::Chat(ChatPatch::Append { .. })))
            .collect();
        assert_eq!(
            appends,
            [&Patch::Chat(ChatPatch::Append {
                turn: 0,
                item: 0,
                text: "Hello there".into(),
            })]
        );
        assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
    }

    #[test]
    fn a_viewer_too_far_behind_gets_the_chat_whole() {
        let mut chat = chat();
        connect(&mut chat, "s");
        let mut viewer = Viewer::new();
        viewer.catch_up(&chat);
        send(&mut chat, "hi");
        for _ in 0..=JOURNAL {
            words(&mut chat, "x");
        }
        assert!(matches!(
            viewer.catch_up(&chat),
            Some(ViewUpdate::Snapshot(_))
        ));
        assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
    }

    #[test]
    fn the_agent_asks_and_the_answer_goes_back() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let options: Vec<PermissionOption> = serde_json::from_str(
            r#"[{"optionId":"yes","name":"Allow","kind":"allow_once"},
                {"optionId":"no","name":"Reject","kind":"reject_once"}]"#,
        )
        .unwrap();
        let tool: ToolCallUpdate =
            serde_json::from_str(r#"{"toolCallId":"t","title":"run\nls"}"#).unwrap();
        chat.ask(Value::from(7), &tool, &options);
        let question = chat.node.question.clone().expect("a question");
        assert_eq!(question.title, "run ls");
        assert_eq!(question.choices[1].kind, ChoiceKind::RejectOnce);
        assert_eq!(
            chat.act(ChatAction::Answer(Some("yes".into()))),
            [Action::Answer {
                request: Value::from(7),
                option: Some("yes".into()),
            }]
        );
        assert_eq!(chat.node.question, None);
        // Nothing asked: an answer goes nowhere.
        assert_eq!(chat.act(ChatAction::Answer(None)), []);
    }

    #[test]
    fn stopping_cancels_turns_down_the_question_or_takes_back_the_message() {
        let mut chat = chat();
        // Not yet sent: taken back.
        send(&mut chat, "hello");
        assert_eq!(chat.act(ChatAction::Stop), []);
        assert_eq!(outcome(&chat.node.turns[0]), Some(&Outcome::Stopped));
        // Sent, with a question waiting: none for it, and a cancel.
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let tool: ToolCallUpdate = serde_json::from_str(r#"{"toolCallId":"t"}"#).unwrap();
        chat.ask(Value::from(3), &tool, &[]);
        assert_eq!(
            chat.act(ChatAction::Stop),
            [
                Action::Answer {
                    request: Value::from(3),
                    option: None,
                },
                Action::Cancel,
            ]
        );
        chat.answered(Ok(StopReason::Cancelled));
        assert_eq!(outcome(&chat.node.turns[1]), Some(&Outcome::Stopped));
    }

    #[test]
    fn the_agent_stopping_ends_the_answer_or_says_so() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t","title":"Run","status":"in_progress"}"#,
        );
        chat.disconnected();
        let turn = &chat.node.turns[0];
        assert_eq!(
            outcome(turn),
            Some(&Outcome::Failed("the agent stopped".into()))
        );
        assert!(matches!(&turn.items[0], Item::Tool(tool) if tool.status == ToolStatus::Failed));
        // Idle, it says so, and the next message opens the session again.
        connect(&mut chat, "");
        chat.disconnected();
        assert!(chat.node.notice.is_some());
        assert_eq!(send(&mut chat, "hi"), [Action::Connect]);
        assert_eq!(chat.node.notice, None);
    }

    #[test]
    fn what_the_agent_writes_keeps_its_lines_but_no_controls() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "a\u{1b}]2;x\u{7}b\n\tc");
        assert_eq!(
            chat.node.turns[0].items,
            [Item::Words("a]2;xb\n\tc".into())]
        );
    }

    #[test]
    fn a_chat_handed_over_keeps_its_turns_and_cuts_off_an_answer() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "hi");
        words(&mut chat, "hello");
        let resumed = HostedChat::resume(chat.hand_over());
        assert_eq!(resumed.session(), Some("s"));
        let turn = &resumed.node.turns[0];
        assert_eq!(turn.items, [Item::Words("hello".into())]);
        assert_eq!(
            outcome(turn),
            Some(&Outcome::Failed("cut off by an upgrade".into()))
        );
        assert_eq!(resumed.node.state, ChatState::Ready);
    }

    fn set(setting: &str, value: &str) -> Action {
        Action::Set {
            setting: setting.into(),
            value: value.into(),
        }
    }

    #[test]
    fn a_new_chat_asks_for_auto_mode_and_choices_outlast_the_agent() {
        let mut chat = chat();
        let opened = |model: &str| Opened {
            session: "s".into(),
            settings: Some(claude_settings(model)),
        };
        assert_eq!(chat.connected(opened("haiku")), [set("mode", "auto")]);
        let modes: Vec<(SettingKind, &str)> = (chat.node.settings.iter())
            .map(|setting| (setting.kind, setting.value.as_str()))
            .collect();
        assert_eq!(
            modes,
            [(SettingKind::Mode, "auto"), (SettingKind::Model, "haiku")]
        );

        // A choice goes to the agent, and the view shows it at once.
        assert_eq!(
            chat.act(ChatAction::Set {
                setting: "model".into(),
                value: "sonnet".into(),
            }),
            [set("model", "sonnet")]
        );
        // A value it doesn't offer goes nowhere.
        assert_eq!(
            chat.act(ChatAction::Set {
                setting: "model".into(),
                value: "gpt".into(),
            }),
            []
        );

        // The agent stops, and comes back having forgotten: the choices go
        // again, but Auto only as the user had it.
        chat.disconnected();
        assert_eq!(
            chat.connected(opened("haiku")),
            [set("mode", "auto"), set("model", "sonnet")]
        );
    }

    #[test]
    fn a_chat_without_auto_asks_for_nothing() {
        let mut chat = chat();
        let settings: Vec<SessionConfigOption> = serde_json::from_str(
            r#"[{"id":"mode","name":"Mode","category":"mode","type":"select","currentValue":"ask",
                 "options":[{"value":"ask","name":"Ask"}]}]"#,
        )
        .unwrap();
        let opened = Opened {
            session: "s".into(),
            settings: Some(settings),
        };
        assert_eq!(chat.connected(opened), []);
    }

    #[test]
    fn usage_and_settings_follow_the_agent() {
        let mut chat = chat();
        connect(&mut chat, "s");
        update(
            &mut chat,
            r#"{"sessionUpdate":"usage_update","used":22118,"size":1000000}"#,
        );
        assert_eq!(
            chat.node.usage,
            Some(Usage {
                used: 22118,
                size: 1_000_000,
            })
        );
        chat.settings_changed(Ok(claude_settings("sonnet")));
        update(
            &mut chat,
            r#"{"sessionUpdate":"current_mode_update","currentModeId":"auto"}"#,
        );
        assert_eq!(chat.node.settings[0].value, "auto");
        assert_eq!(chat.node.settings[1].value, "sonnet");
    }
}
