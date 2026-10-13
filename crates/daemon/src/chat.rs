//! Chats with agents: the daemon's side of a chat pane.
//!
//! A chat has a terminal's ID and goes in panes as terminals do, but it
//! has no grid: it keeps the conversation as turns (a [`ChatNode`]), and
//! clients draw them. Every change to the turns goes into a journal of
//! [`ChatPatch`]es, which each viewer is sent from where it last was.
//!
//! The agent behind the chat is the server's: a chat asks for what it
//! needs with [`Action`]s, and is told what the agent said. Agents differ
//! in what they send: the chat takes what any of them sends, rather than
//! asking which agent it is, so a new agent that sends the same works too.

use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol_schema::MaybeUndefined;
use agent_client_protocol_schema::v1::{
    ContentBlock, PermissionOption, PermissionOptionKind, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOptions, SessionUpdate, StopReason,
    ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
};
use clackatoo_protocol::{
    AgentTools, ChatAction, ChatNode, ChatPatch, ChatState, Choice, ChoiceKind, Diff, Item,
    Location, Node, Outcome, Patch, Question, Sender, Setting, SettingChoice, SettingKind, Size,
    TerminalId, TerminalInfo, TerminalStatus, Tool, ToolContent, ToolKind, ToolStatus, Turn,
    TurnEnd, TurnFiles, Usage, View, ViewUpdate,
};
use serde_json::Value;

use crate::agent::Opened;
use crate::agents::AgentSpec;
use crate::handover;

mod relations;

pub(crate) use relations::{Cause, MAX_INBOX, Message, Relations};

/// The most characters of a title the agent gives a conversation that a
/// chat keeps, for its tab.
const TITLE: usize = 80;

/// How many changes a chat keeps for viewers that are behind. One further
/// behind gets the whole chat again.
const JOURNAL: usize = 4096;

/// About the most a chat's journal keeps, in bytes, so a command's output
/// streamed a piece at a time can't fill it.
const JOURNAL_BYTES: usize = 2 * 1024 * 1024;

/// The most of a tool's text a chat keeps, in bytes: the end of it, as
/// the end of a command's output is where what went wrong is.
const TOOL_TEXT: usize = 32 * 1024;

/// The most of each side of a change to a file a chat keeps, in bytes:
/// the start of it.
const DIFF_TEXT: usize = 256 * 1024;

/// The ID of the chat's own setting for what its agent may do with
/// clackatoo's tools. It's shown with the agent's settings, and kept
/// with the values the user gave them, through upgrades, but never sent
/// to the agent, which has no such setting.
const TOOLS_SETTING: &str = "clackatoo.tools";

/// The values the tools setting takes: each one's ID, what it's called,
/// and what it means.
const TOOLS_VALUES: [(AgentTools, &str, &str, &str); 3] = [
    (
        AgentTools::Ask,
        "ask",
        "Ask",
        "Asks before it runs a command or types into a terminal",
    ),
    (
        AgentTools::Allow,
        "allow",
        "Allow",
        "Runs commands in its own terminals without asking",
    ),
    (
        AgentTools::Off,
        "off",
        "Off",
        "None of clackatoo's tools, from the next session on",
    ),
];

/// The ID a chat keeps, with the values chosen, whether its agent keeps
/// Claude Code's own Task tool, so that it lasts through upgrades and
/// the session is opened again the same way. It's chosen when the chat
/// starts, and never shown or sent to the agent as a setting: Claude
/// Code reads it only as a session opens.
const TASK_SETTING: &str = "clackatoo.task";

/// The most questions a chat keeps waiting. Past it, a tool's question is
/// refused rather than queued.
pub(crate) const MAX_QUESTIONS: usize = 16;

/// The answers to a question about one of clackatoo's tools: its ID, what
/// it's called, and what it means.
const TOOL_CHOICES: [(&str, &str, ChoiceKind); 3] = [
    ("allow", "Allow", ChoiceKind::AllowOnce),
    ("always", "Always allow", ChoiceKind::AllowAlways),
    ("reject", "Reject", ChoiceKind::RejectOnce),
];

/// The answers to a question about the agent messaging or starting
/// another chat. "Always" is for this chat and that one alone: it never
/// changes the chat's Tools setting, so no chat gains, through another,
/// what its own setting doesn't give it.
const TALK_CHOICES: [(&str, &str, ChoiceKind); 3] = [
    ("allow", "Allow", ChoiceKind::AllowOnce),
    (
        "pair",
        "Always allow for that chat",
        ChoiceKind::AllowAlways,
    ),
    ("reject", "Deny", ChoiceKind::RejectOnce),
];

/// Which answers a tool's question offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choices {
    /// Allow, Always allow, which lets the agent run commands in its own
    /// terminals, and Reject.
    Tool,
    /// Allow, Always allow for the chat it's about, and Deny.
    Talk,
}

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
    /// Let a call to one of clackatoo's tools go ahead, numbered as the
    /// server numbered it, or refuse it; with `pair`, a call about another
    /// chat goes ahead without asking from now on, for that chat.
    Permit { call: u64, allow: bool, pair: bool },
    /// Change one of the agent's settings for the chat.
    Set { setting: String, value: String },
    /// Install the chat's agent, as the user agreed to.
    Install,
}

/// The answer to an install offer that installs.
pub(crate) const INSTALL: &str = "install";

/// Files a tool changed on disk itself, as the agent says once it has,
/// or, while it runs, files it's about to change.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Wrote {
    /// Whether the tool has finished: if not, it names files it's going
    /// to change, which it hasn't yet, or not all.
    pub(crate) done: bool,
    /// The turn it's in.
    pub(crate) turn: usize,
    /// Its ID.
    pub(crate) tool: String,
    /// Where it is among its turn's items, so its changes can be put in
    /// the order the agent made them.
    pub(crate) order: usize,
    /// The files, as the agent names them: absolute, or from the chat's
    /// working directory.
    pub(crate) paths: Vec<String>,
    /// What it says it changed in them, as last said, whole: the changes
    /// to check the files against.
    pub(crate) diffs: Vec<RawDiff>,
}

/// A change a tool says it made to a file: `old` replaced by `new`, as
/// the agent gives it, with no line cut. `old` is `None` for a file the
/// tool made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawDiff {
    pub(crate) path: String,
    pub(crate) old: Option<String>,
    pub(crate) new: String,
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
    /// The agent's name, which the server finds it by.
    agent: String,
    /// The agent's session, once it has made one.
    session: Option<String>,
    /// Whether the session is open in the agent now. It isn't before it's
    /// made, after an upgrade, or once the agent has gone.
    link: Link,
    /// A message sent while there's no session, to go once there is.
    queued: Option<String>,
    node: ChatNode,
    /// The questions waiting for answers, the first shown: the agent may
    /// ask about several tools at once, and clackatoo's own tools ask
    /// too.
    asking: VecDeque<(Asker, Question)>,
    /// The number the next question gets.
    next_question: u64,
    /// The values the user gave settings, by their IDs, which the chat
    /// asks for again whenever its session opens: the agent forgets them
    /// when it stops.
    chosen: Vec<(String, String)>,
    /// The mode a new chat asks for when its session first opens.
    starting_mode: Option<String>,
    journal: Journal,
    /// What each tool of the turn being answered last said it changed in
    /// files, by its ID.
    diffs: Vec<(String, Vec<RawDiff>)>,
    /// Where it stands among the chats agents talk in.
    relations: Relations,
}

/// Who asked a question in a chat, and so gets the answer.
#[derive(Clone, Debug, PartialEq)]
enum Asker {
    /// The agent, in the request with this JSON-RPC ID.
    Agent(Value),
    /// One of clackatoo's tools the agent called, by the server's number
    /// for the call.
    Tool(u64),
    /// The daemon: whether to install the agent, which the chat waits
    /// for.
    Install,
    /// The daemon: whether to install the agent, which runs another way
    /// meanwhile, so nothing waits for the answer.
    Suggest,
}

/// The changes made to a chat, numbered, for viewers to catch up with.
#[derive(Debug, Default)]
struct Journal {
    /// The number of the first change kept.
    first: u64,
    patches: VecDeque<ChatPatch>,
    /// About how much the changes kept take, in bytes.
    bytes: usize,
}

impl Journal {
    /// Keeps `patch`, and lets the oldest go past the journal's bounds, of
    /// changes and of bytes: a viewer that far behind gets the chat whole.
    fn push(&mut self, patch: ChatPatch) {
        self.bytes += weight(&patch);
        self.patches.push_back(patch);
        while self.patches.len() > 1 && (self.patches.len() > JOURNAL || self.bytes > JOURNAL_BYTES)
        {
            let gone = self.patches.pop_front().expect("more than one");
            self.bytes -= weight(&gone);
            self.first += 1;
        }
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
    /// A chat with `agent`, in `cwd`, not yet with a session: the server
    /// is to get it one. It's named after the agent until the agent names
    /// the conversation. Its agent may do what `tools` says with
    /// clackatoo's tools, and keeps Claude Code's Task tool if `task`.
    pub(crate) fn new(
        id: TerminalId,
        size: Size,
        cwd: String,
        agent: &AgentSpec,
        tools: AgentTools,
        task: bool,
    ) -> Self {
        Self {
            id,
            size,
            title: agent.title.clone(),
            cwd,
            agent: agent.name.clone(),
            session: None,
            link: Link::Opening,
            queued: None,
            node: ChatNode {
                agent: agent.title.clone(),
                settings: vec![tools_setting(tools)],
                ..ChatNode::default()
            },
            asking: VecDeque::new(),
            next_question: 0,
            chosen: vec![
                (TOOLS_SETTING.to_owned(), tools_value(tools).to_owned()),
                (
                    TASK_SETTING.to_owned(),
                    if task { "on" } else { "off" }.to_owned(),
                ),
            ],
            starting_mode: agent.starting_mode.clone(),
            journal: Journal::default(),
            diffs: Vec::new(),
            relations: Relations::default(),
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
        let senders: Vec<(usize, &Sender)> = (self.node.turns.iter().enumerate())
            .filter_map(|(index, turn)| Some((index, turn.from.as_ref()?)))
            .collect();
        let relations = self.relations.hand_over(&senders);
        handover::Chat {
            id: self.id.0,
            cols: self.size.cols(),
            rows: self.size.rows(),
            title: self.title,
            cwd: self.cwd,
            session: self.session,
            failure,
            turns: self.node.turns.into_iter().map(turn_to_handover).collect(),
            // The chat's own setting is kept with the values chosen.
            settings: (self.node.settings.into_iter())
                .filter(|setting| setting.id != TOOLS_SETTING)
                .map(setting_to_handover)
                .collect(),
            chosen: self.chosen,
            agent: self.agent,
            relations,
        }
    }

    /// A chat the last build handed over, with `agent`, which is what
    /// its name is now.
    pub(crate) fn resume(chat: handover::Chat, agent: &AgentSpec) -> Self {
        let size = Size::new(chat.cols, chat.rows).unwrap_or(Size::new(80, 24).expect("valid"));
        let mut resumed = Self::new(
            TerminalId(chat.id),
            size,
            chat.cwd,
            agent,
            AgentTools::Ask,
            true,
        );
        resumed.link = Link::Closed;
        resumed.title = chat.title;
        resumed.session = chat.session;
        resumed.node.turns = chat.turns.into_iter().map(turn_from_handover).collect();
        resumed.node.settings = (chat.settings.into_iter())
            .map(setting_from_handover)
            .collect();
        resumed.chosen = chat.chosen;
        let (relations, senders) = Relations::resume(chat.relations);
        resumed.relations = relations;
        for (index, sender) in senders {
            if let Some(turn) = resumed.node.turns.get_mut(index) {
                turn.from = Some(sender);
            }
        }
        (resumed.node.settings).push(tools_setting(resumed.tools()));
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

    /// The agent's name.
    pub(crate) fn agent(&self) -> &str {
        &self.agent
    }

    /// The size of the pane it's in.
    pub(crate) fn size(&self) -> Size {
        self.size
    }

    /// What its agent may do with clackatoo's tools. A chat from before
    /// they could be turned off may use them, asking first.
    pub(crate) fn tools(&self) -> AgentTools {
        let value =
            (self.chosen.iter()).find_map(|(id, value)| (id == TOOLS_SETTING).then_some(value));
        match value.map(String::as_str) {
            Some("off") => AgentTools::Off,
            Some("allow") => AgentTools::Allow,
            _ => AgentTools::Ask,
        }
    }

    /// Whether its agent keeps Claude Code's own Task tool. A chat from
    /// before it could be taken away keeps it.
    pub(crate) fn task(&self) -> bool {
        !(self.chosen.iter()).any(|(id, value)| id == TASK_SETTING && value == "off")
    }

    /// Lets its agent do what `tools` says with clackatoo's tools, from
    /// now on.
    fn set_tools(&mut self, tools: AgentTools) {
        self.chosen.retain(|(id, _)| id != TOOLS_SETTING);
        (self.chosen).push((TOOLS_SETTING.to_owned(), tools_value(tools).to_owned()));
        self.set_value(TOOLS_SETTING, tools_value(tools));
    }

    /// The agent's session, if it has made one.
    pub(crate) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub(crate) fn view(&self) -> View {
        View {
            title: self.title.clone(),
            cwd: None,
            root: Node::Chat(Box::new(self.node.clone())),
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
        let mut chat: Vec<Option<ChatPatch>> = Vec::new();
        // Where each item's first whole value in this update is, which a
        // later one takes the place of.
        let mut items: HashMap<(u32, u32), usize> = HashMap::new();
        for patch in self.journal.patches.iter().skip(skip) {
            match patch {
                ChatPatch::Turn { index, .. } => items.retain(|(turn, _), _| turn != index),
                ChatPatch::Item { turn, item, .. } => {
                    if let Some(&at) = items.get(&(*turn, *item)) {
                        // What was added to it since is in this value.
                        for later in &mut chat[at + 1..] {
                            if later
                                .as_ref()
                                .is_some_and(|later| adds_to(later, *turn, *item))
                            {
                                *later = None;
                            }
                        }
                        chat[at] = Some(patch.clone());
                        continue;
                    }
                    items.insert((*turn, *item), chat.len());
                }
                _ => {}
            }
            // Pieces of the same words, or output, streamed, go as one.
            if let Some(Some(last)) = chat.last_mut()
                && merge(last, patch)
            {
                continue;
            }
            chat.push(Some(patch.clone()));
        }
        patches.extend(chat.into_iter().flatten().map(Patch::Chat));
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
            // Only an answer to the question shown counts: one sent twice,
            // or as the question was taken back, isn't the next one's.
            ChatAction::Answer { question, option } => {
                if (self.asking.front()).is_some_and(|(_, asked)| asked.id == question)
                    && let Some((asker, _)) = self.asking.pop_front()
                {
                    self.show_question();
                    actions.extend(self.answer(asker, option));
                }
            }
            // The chat's own: the agent never hears of it.
            ChatAction::Set { setting, value } if setting == TOOLS_SETTING => {
                if let Some(&(tools, ..)) = (TOOLS_VALUES.iter()).find(|(_, id, ..)| *id == value) {
                    self.set_tools(tools);
                }
            }
            // The server's to do: they're about files, not the agent.
            ChatAction::Undo { .. } | ChatAction::Redo { .. } => {}
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
        // The user's own: what agents do of it comes of this turn.
        self.relations.hops = 0;
        let turn = u32::try_from(self.node.turns.len()).unwrap_or(u32::MAX);
        self.relations.cause = Some((self.id, turn));
        self.begin(text.clone(), text, None, actions);
    }

    /// Starts a turn: shows `shown`, sent by `from` if another chat's
    /// agent sent it, and sends the agent `prompt`.
    fn begin(
        &mut self,
        shown: String,
        prompt: String,
        from: Option<Sender>,
        actions: &mut Vec<Action>,
    ) {
        self.set_notice(None);
        self.push_turn(Turn {
            prompt: shown,
            sent: now(),
            items: Vec::new(),
            end: None,
            files: None,
            from,
        });
        match self.link {
            Link::Open => {
                self.set_state(ChatState::Answering);
                actions.push(Action::Prompt(prompt));
            }
            Link::Opening => self.queued = Some(prompt),
            Link::Closed => {
                self.queued = Some(prompt);
                self.link = Link::Opening;
                self.set_state(ChatState::Starting);
                actions.push(Action::Connect);
            }
        }
    }

    // --- Other agents -------------------------------------------------------

    /// Where the chat stands among the chats agents talk in.
    pub(crate) fn relations(&self) -> &Relations {
        &self.relations
    }

    /// The chat was started by the agent of the chat `parent`, which goes
    /// back to the user's chat `root`, `depth` deep. It starts in `mode`,
    /// the mode its parent was in, or with none, in its agent's own: never
    /// in clackatoo's choice for a new chat, which may let it do more than
    /// its parent can.
    pub(crate) fn started_by(
        &mut self,
        parent: TerminalId,
        root: TerminalId,
        depth: u8,
        mode: Option<String>,
    ) {
        self.relations.parent = Some(parent);
        self.relations.root = Some(root);
        self.relations.depth = depth;
        self.starting_mode = mode;
    }

    /// The mode the agent is in, by its value, if it offers modes.
    pub(crate) fn mode(&self) -> Option<&str> {
        (self.node.settings.iter())
            .find(|setting| setting.kind == SettingKind::Mode)
            .map(|setting| setting.value.as_str())
    }

    /// Takes `message`, from another chat's agent: it goes at once, as a
    /// turn of its own, unless the agent is answering. Then it waits for
    /// the turn to end, behind any others waiting; with
    /// [`MAX_INBOX`] waiting already, it's handed back. Returns what the
    /// server must do to send it, if it went.
    pub(crate) fn deliver(&mut self, message: Message) -> Result<Vec<Action>, Message> {
        if self.answering() || !self.relations.inbox.is_empty() {
            if self.relations.inbox.len() >= MAX_INBOX {
                return Err(message);
            }
            self.relations.inbox.push_back(message);
            return Ok(Vec::new());
        }
        Ok(self.begin_message(message))
    }

    /// Sends the first message waiting from another agent, if the agent
    /// has finished answering. Returns what the server must do to send it.
    pub(crate) fn deliver_next(&mut self) -> Vec<Action> {
        if self.answering() {
            return Vec::new();
        }
        match self.relations.inbox.pop_front() {
            Some(message) => self.begin_message(message),
            None => Vec::new(),
        }
    }

    /// Starts a turn with `message`, from another agent.
    fn begin_message(&mut self, message: Message) -> Vec<Action> {
        let mut actions = Vec::new();
        self.relations.hops = message.hops;
        self.relations.cause = message.cause;
        let prompt = relations::prompt_for(&message);
        self.begin(message.text, prompt, Some(message.from), &mut actions);
        actions
    }

    /// Whether a message has been sent and not yet answered.
    pub(crate) fn is_answering(&self) -> bool {
        self.answering()
    }

    /// The conversation as it is now.
    pub(crate) fn node(&self) -> &ChatNode {
        &self.node
    }

    /// The chat as other chats' agents know it.
    pub(crate) fn sender(&self) -> Sender {
        Sender {
            chat: self.id,
            agent: self.node.agent.clone(),
            title: self.title.clone(),
        }
    }

    /// Stops the agent answering, or takes back the message if it hasn't
    /// gone yet.
    fn stop(&mut self, actions: &mut Vec<Action>) {
        if !self.answering() {
            return;
        }
        // Questions waiting are answered with none, as the protocol asks.
        // An offer to install is turned down, and a suggestion to install,
        // which nothing waits for, stays.
        for (asker, question) in std::mem::take(&mut self.asking) {
            match asker {
                Asker::Suggest => self.asking.push_back((asker, question)),
                Asker::Install => {
                    self.link = Link::Closed;
                    self.set_state(ChatState::Ready);
                }
                asker => actions.extend(self.answer(asker, None)),
            }
        }
        self.show_question();
        if self.link == Link::Open {
            actions.push(Action::Cancel);
        } else if self.queued.take().is_some() {
            self.end_turn(Outcome::Stopped);
        }
        // The user wants the agent to stop: messages from other agents
        // waiting their turn don't start it again.
        let dropped = std::mem::take(&mut self.relations.inbox).len();
        if dropped > 0 {
            let plural = if dropped == 1 { "" } else { "s" };
            self.set_notice(Some(format!(
                "Stopped: {dropped} message{plural} from other agents waiting their turn \
                 weren't sent."
            )));
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
            self.chosen.push((setting.id.clone(), mode));
        }
        let mut actions = Vec::new();
        for (setting, value) in self.chosen.clone() {
            if setting == TOOLS_SETTING || setting == TASK_SETTING {
                continue;
            }
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

    /// The chat's agent isn't installed: asks, in `what`, whether to
    /// install it, and holds any message until it's answered.
    pub(crate) fn offer_install(&mut self, what: &str) {
        self.ask_to_install(Asker::Install, what);
    }

    /// Asks `what`, whether to install the chat's agent, for `asker`.
    fn ask_to_install(&mut self, asker: Asker, what: &str) {
        let question = Question {
            id: self.next_question,
            title: clean(what),
            choices: vec![
                Choice {
                    id: INSTALL.to_owned(),
                    name: "Install".to_owned(),
                    kind: ChoiceKind::AllowOnce,
                },
                Choice {
                    id: "later".to_owned(),
                    name: "Not now".to_owned(),
                    kind: ChoiceKind::RejectOnce,
                },
            ],
        };
        self.next_question += 1;
        self.asking.push_back((asker, question));
        self.show_question();
    }

    /// Takes back an offer to install the chat's agent, if one is asked.
    fn withdraw_install(&mut self) {
        (self.asking).retain(|(asker, _)| !matches!(asker, Asker::Install | Asker::Suggest));
        self.show_question();
    }

    /// Whether the chat is asking whether to install its agent.
    pub(crate) fn offering_install(&self) -> bool {
        (self.asking.iter()).any(|(asker, _)| matches!(asker, Asker::Install | Asker::Suggest))
    }

    /// Whether the chat's session is open.
    pub(crate) fn is_connected(&self) -> bool {
        self.link == Link::Open
    }

    /// The chat's agent runs, but not from where clackatoo installs it:
    /// suggests, in `what`, installing it there, waiting for nothing.
    pub(crate) fn suggest_install(&mut self, what: &str) {
        if !self.asking.is_empty() {
            return;
        }
        self.ask_to_install(Asker::Suggest, what);
    }

    /// The chat's agent, which runs another way meanwhile, has been
    /// installed, or couldn't be: why.
    pub(crate) fn installed_meanwhile(&mut self, outcome: &Result<(), String>) {
        self.withdraw_install();
        self.set_notice(outcome.as_ref().err().cloned());
    }

    /// The chat's agent is being installed, as another chat asked: it
    /// waits for that.
    pub(crate) fn installing(&mut self) {
        self.set_state(ChatState::Starting);
        self.set_notice(Some(format!("Installing {}…", self.node.agent)));
    }

    /// The chat's agent has been installed: the server gets it a session
    /// next.
    pub(crate) fn installed(&mut self) {
        self.withdraw_install();
        self.link = Link::Opening;
        self.set_notice(None);
    }

    /// The session couldn't be made or opened, nor the agent installed.
    pub(crate) fn failed_to_connect(&mut self, reason: &str) {
        self.set_notice(None);
        self.withdraw_install();
        self.link = Link::Closed;
        if self.queued.take().is_some() {
            self.end_turn(Outcome::Failed(format!(
                "{} couldn't start: {reason}",
                self.node.agent
            )));
        }
        self.set_state(ChatState::Failed(reason.to_owned()));
    }

    /// The agent has gone: it exited, or stopped answering. If it never
    /// started, `why` says why, as far as it said.
    pub(crate) fn disconnected(&mut self, why: Option<&str>) {
        let was_opening = self.link == Link::Opening;
        self.link = Link::Closed;
        let queued = self.queued.take();
        if let Some(why) = why.filter(|_| was_opening) {
            let why = format!("it stopped, saying: {why}");
            if queued.is_some() {
                self.end_turn(Outcome::Failed(format!(
                    "{} couldn't start: {why}",
                    self.node.agent
                )));
            }
            self.set_state(ChatState::Failed(why));
        } else if self.answering() {
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

    /// The option that lets `tool` go ahead once, if the agent asks
    /// whether it may use one of clackatoo's own tools: those ask the
    /// user themselves when they must, so the agent's question is answered
    /// at once, and the user isn't asked twice. Codex asks about every
    /// MCP tool call, as a call titled `mcp.clackatoo.TOOL`, with the
    /// server in its input; Claude Code is told not to ask.
    pub(crate) fn about_own_tools(
        &self,
        tool: &ToolCallUpdate,
        options: &[PermissionOption],
    ) -> Option<String> {
        let id = tool.tool_call_id.0.to_string();
        let title = (self.tool(&id).map(|tool| tool.title.clone()))
            .or_else(|| tool.fields.title.clone())
            .unwrap_or_default();
        let input = tool.fields.raw_input.as_ref();
        let server = input
            .and_then(|input| input.get("server").or_else(|| input.get("serverName")))
            .and_then(Value::as_str);
        let name = crate::mcp::SERVER_NAME;
        let ours = server == Some(name)
            || title.starts_with(&format!("mcp.{name}."))
            || title.starts_with(&format!("mcp__{name}__"));
        if !ours || self.tools() == AgentTools::Off {
            return None;
        }
        (options.iter())
            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
            .map(|option| option.option_id.0.to_string())
    }

    /// The agent asks whether it may use a tool.
    pub(crate) fn ask(
        &mut self,
        request: Value,
        tool: &ToolCallUpdate,
        options: &[PermissionOption],
    ) {
        // The tool's own title, if it's been seen: Codex's questions call
        // every command "Run command", but its tool call has the command.
        let id = tool.tool_call_id.0.to_string();
        let title = (self.tool(&id).map(|tool| tool.title.clone()))
            .or_else(|| tool.fields.title.clone())
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
        let question = Question {
            id: self.next_question,
            title: clean(&title),
            choices,
        };
        self.next_question += 1;
        self.asking.push_back((Asker::Agent(request), question));
        self.show_question();
    }

    /// One of clackatoo's tools asks whether the agent may do `what`, as
    /// "run `make` in a terminal", for the call the server numbered
    /// `call`. The answer comes back as [`Action::Permit`]. `whole` is
    /// what the agent would type, if it's too long for the question: it
    /// goes in the conversation, in full, for the user to read first.
    /// Returns whether it was asked: too many questions waiting refuse it.
    pub(crate) fn ask_for_tool(
        &mut self,
        call: u64,
        what: &str,
        whole: Option<&str>,
        offered: Choices,
    ) -> bool {
        if self.asking.len() >= MAX_QUESTIONS {
            return false;
        }
        if let Some(whole) = whole {
            self.set_tool(Tool {
                id: format!("clackatoo-{call}"),
                title: "What the agent asks to type, in full".to_owned(),
                status: ToolStatus::Done,
                kind: ToolKind::Other,
                content: vec![ToolContent::Text(format!("```\n{whole}\n```"))],
                locations: Vec::new(),
            });
        }
        let choices = match offered {
            Choices::Tool => TOOL_CHOICES,
            Choices::Talk => TALK_CHOICES,
        };
        let choices = (choices.iter())
            .map(|&(id, name, kind)| Choice {
                id: id.to_owned(),
                name: name.to_owned(),
                kind,
            })
            .collect();
        // Shown as given: its control characters are already made
        // visible, and nothing of it may go unseen.
        let question = Question {
            id: self.next_question,
            title: what.to_owned(),
            choices,
        };
        self.next_question += 1;
        self.asking.push_back((Asker::Tool(call), question));
        self.show_question();
        true
    }

    /// Takes back the question about the tool call `call`, which no longer
    /// waits for an answer: the agent cancelled it, or went.
    pub(crate) fn withdraw(&mut self, call: u64) {
        self.asking.retain(|(asker, _)| *asker != Asker::Tool(call));
        self.show_question();
    }

    /// What to do about the answer `option` to `asker`'s question.
    /// "Always allow" for clackatoo's tools lets the agent run commands in
    /// its own terminals without asking from then on, until the user
    /// changes the chat's Tools setting back. "Always allow for that
    /// chat" is the server's to remember, and changes no setting.
    fn answer(&mut self, asker: Asker, option: Option<String>) -> Option<Action> {
        let install = option.as_deref() == Some(INSTALL);
        match asker {
            Asker::Agent(request) => Some(Action::Answer { request, option }),
            Asker::Tool(call) => {
                let option = option.as_deref();
                if option == Some("always") {
                    self.set_tools(AgentTools::Allow);
                }
                Some(Action::Permit {
                    call,
                    allow: matches!(option, Some("allow" | "always" | "pair")),
                    pair: option == Some("pair"),
                })
            }
            Asker::Install if install => {
                self.installing();
                Some(Action::Install)
            }
            Asker::Install => {
                self.failed_to_connect(
                    "it isn't installed. The next message offers to install it again.",
                );
                None
            }
            Asker::Suggest if install => {
                self.set_notice(Some(format!(
                    "Installing {}'s adapter. The chat carries on meanwhile.",
                    self.node.agent
                )));
                Some(Action::Install)
            }
            Asker::Suggest => None,
        }
    }

    /// Shows the first question waiting, if any.
    fn show_question(&mut self) {
        let question = self.asking.front().map(|(_, question)| question.clone());
        self.set_question(question);
    }

    /// Takes a change to the session. Returns the files a tool that has
    /// finished says it changed, if the change says so: they're on disk
    /// already, as Claude Code's tools write them.
    pub(crate) fn update(&mut self, update: SessionUpdate) -> Option<Wrote> {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => self.write(&chunk.content, false),
            SessionUpdate::AgentThoughtChunk(chunk) => self.write(&chunk.content, true),
            SessionUpdate::ToolCall(call) => {
                self.note_diffs(&call.tool_call_id.0, &call.content);
                let tool = Tool {
                    id: call.tool_call_id.0.to_string(),
                    title: clean(&call.title),
                    status: tool_status(call.status),
                    kind: tool_kind(call.kind),
                    content: tool_content(&call.content),
                    locations: tool_locations(&call.locations),
                };
                let (id, wrote) = (tool.id.clone(), self.wrote(&tool));
                self.set_tool(tool);
                self.add_output(&id, call.meta.as_ref());
                return wrote;
            }
            SessionUpdate::ToolCallUpdate(update) => {
                if let Some(content) = &update.fields.content {
                    self.note_diffs(&update.tool_call_id.0, content);
                }
                let id = update.tool_call_id.0.to_string();
                let known = self.tool(&id).cloned();
                let mut tool = known.clone().unwrap_or(Tool {
                    id: id.clone(),
                    title: "a tool".to_owned(),
                    status: ToolStatus::Running,
                    kind: ToolKind::Other,
                    content: Vec::new(),
                    locations: Vec::new(),
                });
                if let Some(title) = &update.fields.title {
                    tool.title = clean(title);
                }
                if let Some(status) = update.fields.status {
                    tool.status = tool_status(status);
                }
                if let Some(kind) = update.fields.kind {
                    tool.kind = tool_kind(kind);
                }
                // What it shows is replaced, not added to: a command's
                // description gives way to its output. Output streamed
                // beside stays.
                if let Some(content) = &update.fields.content {
                    let streamed = (tool.content.drain(..))
                        .filter(|content| matches!(content, ToolContent::Output(_)));
                    let streamed: Vec<ToolContent> = streamed.collect();
                    tool.content = tool_content(content);
                    tool.content.extend(streamed);
                }
                if let Some(locations) = &update.fields.locations {
                    tool.locations = tool_locations(locations);
                }
                // Claude Code's adapter says what a tool wrote once it has
                // written it, in an update of its own that may come before
                // or after the one saying it's finished.
                let fields = &update.fields;
                let says = fields.status.is_some()
                    || fields.content.is_some()
                    || fields.locations.is_some();
                let wrote = says.then(|| self.wrote(&tool)).flatten();
                // A piece of output alone changes nothing else, and goes
                // as an append, not the tool again.
                if known.as_ref() != Some(&tool) {
                    self.set_tool(tool);
                }
                self.add_output(&id, update.meta.as_ref());
                return wrote;
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                // Not a change to the turns: viewers pick it up anyway.
                // Codex's is the whole of the first message.
                if let MaybeUndefined::Value(title) = info.title {
                    self.title = clean(&title);
                    if let Some((cut, _)) = self.title.char_indices().nth(TITLE) {
                        self.title.truncate(cut);
                        self.title.push('…');
                    }
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
        None
    }

    /// Notes the changes to files in `content`, which tool `id` shows,
    /// if it shows any, in place of what it showed before.
    fn note_diffs(&mut self, id: &str, content: &[ToolCallContent]) {
        let diffs: Vec<RawDiff> = (content.iter())
            .filter_map(|content| match content {
                ToolCallContent::Diff(diff) => Some(RawDiff {
                    path: diff.path.to_string_lossy().into_owned(),
                    old: diff.old_text.clone(),
                    new: diff.new_text.clone(),
                }),
                _ => None,
            })
            .collect();
        if diffs.is_empty() {
            return;
        }
        self.diffs.retain(|(tool, _)| tool != id);
        self.diffs.push((id.to_owned(), diffs));
    }

    /// The files `tool` changed, if it's a finished edit that names any,
    /// or the files it's about to change, if it's one still running.
    fn wrote(&self, tool: &Tool) -> Option<Wrote> {
        let diffs = (tool.content.iter()).filter_map(|content| match content {
            ToolContent::Diff(diff) => Some(&diff.path),
            ToolContent::Text(_) | ToolContent::Output(_) => None,
        });
        let edits = matches!(tool.kind, ToolKind::Edit | ToolKind::Delete)
            || diffs.clone().next().is_some();
        if tool.status == ToolStatus::Failed || !edits {
            return None;
        }
        let mut paths: Vec<String> = Vec::new();
        let named = diffs.chain(tool.locations.iter().map(|location| &location.path));
        for path in named {
            if !path.is_empty() && !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        // A tool saying what it wrote after its turn ended still wrote it,
        // in the last turn.
        let turn = self.node.turns.len().checked_sub(1)?;
        let items = &self.node.turns[turn].items;
        let order = (items.iter())
            .position(|item| matches!(item, Item::Tool(old) if old.id == tool.id))
            .unwrap_or(items.len());
        let diffs = (self.diffs.iter())
            .find(|(id, _)| *id == tool.id)
            .map(|(_, diffs)| diffs.clone())
            .unwrap_or_default();
        Some(Wrote {
            done: tool.status == ToolStatus::Done,
            turn,
            tool: tool.id.clone(),
            order,
            paths,
            diffs,
        })
        .filter(|wrote| !wrote.paths.is_empty())
    }

    /// Whether `update`, not yet taken, says a tool that may change files
    /// has finished (see [`writes_files`]): whatever it changed on disk is
    /// there by then.
    pub(crate) fn finishes_writing(&self, update: &SessionUpdate) -> bool {
        let finished = |status: ToolCallStatus| {
            matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
        };
        match update {
            SessionUpdate::ToolCall(call) => {
                finished(call.status) && writes_files(tool_kind(call.kind))
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let kind = (update.fields.kind.map(tool_kind))
                    .or_else(|| self.tool(&update.tool_call_id.0).map(|tool| tool.kind));
                update.fields.status.is_some_and(finished) && kind.is_none_or(writes_files)
            }
            _ => false,
        }
    }

    /// Whether a tool of the agent's that may change files is running now,
    /// as an edit or `cargo fmt` does, while nothing waits on a question
    /// the agent, or one of clackatoo's tools, has asked. A tool waiting to
    /// be allowed isn't running, and while one is asked about, none is
    /// taken to be.
    pub(crate) fn writing(&self) -> bool {
        let asked = (self.asking.iter())
            .any(|(asker, _)| matches!(asker, Asker::Agent(_) | Asker::Tool(_)));
        let Some(turn) = self.answering_turn().filter(|_| !asked) else {
            return false;
        };
        self.node.turns[turn].items.iter().any(|item| {
            matches!(item, Item::Tool(tool)
                if tool.status == ToolStatus::Running && writes_files(tool.kind))
        })
    }

    /// The agent, as it's shown: "Claude Code", "Codex"…
    pub(crate) fn agent_title(&self) -> &str {
        &self.node.agent
    }

    /// Says `notice` in the chat, until the next message.
    pub(crate) fn notify(&mut self, notice: String) {
        self.set_notice(Some(notice));
    }

    /// How many turns the chat has.
    pub(crate) fn turns(&self) -> usize {
        self.node.turns.len()
    }

    /// Says that turn `turn` changed `files`.
    pub(crate) fn set_files(&mut self, turn: usize, files: Option<TurnFiles>) {
        if let Some(edited) = self.node.turns.get_mut(turn)
            && edited.files != files
        {
            edited.files = files;
            self.journal.push(ChatPatch::Files {
                turn: index_u32(turn),
                files,
            });
        }
    }

    /// Where the agent is in its work, for what it writes now: the turn
    /// it's answering, or else the last, and the tool it's running, if one
    /// is.
    pub(crate) fn working_on(&self) -> (usize, Option<String>) {
        let turn = self.node.turns.len().saturating_sub(1);
        let running = (self
            .answering_turn()
            .map(|turn| &self.node.turns[turn].items))
        .and_then(|items| {
            items.iter().rev().find_map(|item| match item {
                Item::Tool(tool) if tool.status == ToolStatus::Running => Some(tool.id.clone()),
                _ => None,
            })
        });
        (turn, running)
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
            let item = if thinking {
                Item::Thought(String::new())
            } else {
                Item::Words(String::new())
            };
            items.push(item.clone());
            let at = items.len() - 1;
            self.patch_item(index, at, item);
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

    /// Adds to the output of tool `id`'s command what `meta` streams, as
    /// Codex's adapter streams it (and Zed's convention, `terminal_output`),
    /// rather than in the tool's content. Each piece goes to viewers as an
    /// append. Past twice what a tool keeps, it's cut back to the end of
    /// it, which goes as the tool again.
    fn add_output(&mut self, id: &str, meta: Option<&serde_json::Map<String, Value>>) {
        let Some(meta) = meta else { return };
        let data = ["terminal_output_delta", "terminal_output"]
            .iter()
            .find_map(|key| meta.get(*key)?.get("data")?.as_str());
        let (Some(data), Some(turn)) = (data, self.answering_turn()) else {
            return;
        };
        let items = &mut self.node.turns[turn].items;
        let Some((at, tool)) = (items.iter_mut().enumerate()).find_map(|(at, item)| match item {
            Item::Tool(tool) if tool.id == id => Some((at, tool)),
            _ => None,
        }) else {
            return;
        };
        let text = clean_text(data);
        tool.append_output(&text);
        let output = tool.output_mut().expect("just added to");
        if output.len() > 2 * TOOL_TEXT {
            *output = tail(output, TOOL_TEXT);
            let tool = Item::Tool(tool.clone());
            self.patch_item(turn, at, tool);
        } else {
            self.journal.push(ChatPatch::Output {
                turn: index_u32(turn),
                item: index_u32(at),
                text,
            });
        }
    }

    /// Adds `tool` to the turn answering, or changes the one with its ID.
    fn set_tool(&mut self, tool: Tool) {
        let Some(index) = self.answering_turn() else {
            return;
        };
        let items = &mut self.node.turns[index].items;
        let at =
            (items.iter()).position(|item| matches!(item, Item::Tool(old) if old.id == tool.id));
        let at = if let Some(at) = at {
            items[at] = Item::Tool(tool.clone());
            at
        } else {
            items.push(Item::Tool(tool.clone()));
            items.len() - 1
        };
        // Only the tool goes, not its turn: a turn of many tools, each
        // with its output, would be sent again with every change.
        self.patch_item(index, at, Item::Tool(tool));
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

    /// Whether a message has been sent and not yet answered: the agent's
    /// turn is running.
    pub(crate) fn answering(&self) -> bool {
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
        // The agent's questions went with the turn. Those of clackatoo's
        // tools wait for their calls to be answered or cancelled, and an
        // offer to install stays until it's answered.
        (self.asking).retain(|(asker, _)| !matches!(asker, Asker::Agent(_)));
        self.show_question();
    }

    fn push_turn(&mut self, turn: Turn) {
        self.diffs.clear();
        self.node.turns.push(turn);
        self.patch_turn(self.node.turns.len() - 1);
    }

    /// Records that item `item` of turn `turn` is now `value`.
    fn patch_item(&mut self, turn: usize, item: usize, value: Item) {
        self.journal.push(ChatPatch::Item {
            turn: index_u32(turn),
            item: index_u32(item),
            value,
        });
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

    /// The agent's settings are `settings`, and the chat's own goes after
    /// them.
    fn set_settings(&mut self, mut settings: Vec<Setting>) {
        settings.push(tools_setting(self.tools()));
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

/// Whether `patch` adds to item `item` of turn `turn`.
fn adds_to(patch: &ChatPatch, turn: u32, item: u32) -> bool {
    matches!(patch, ChatPatch::Append { turn: t, item: i, .. } | ChatPatch::Output { turn: t, item: i, .. }
        if (*t, *i) == (turn, item))
}

/// Adds `patch` to `last`, if both add to the same item the same way.
/// Returns whether it did.
fn merge(last: &mut ChatPatch, patch: &ChatPatch) -> bool {
    match (last, patch) {
        (
            ChatPatch::Append { turn, item, text },
            ChatPatch::Append {
                turn: next_turn,
                item: next_item,
                text: next,
            },
        )
        | (
            ChatPatch::Output { turn, item, text },
            ChatPatch::Output {
                turn: next_turn,
                item: next_item,
                text: next,
            },
        ) if (*turn, *item) == (*next_turn, *next_item) => {
            text.push_str(next);
            true
        }
        _ => false,
    }
}

/// About how many bytes `patch` takes.
fn weight(patch: &ChatPatch) -> usize {
    let item = |item: &Item| match item {
        Item::Words(text) | Item::Thought(text) => text.len(),
        Item::Tool(tool) => {
            let content: usize = (tool.content.iter())
                .map(|content| match content {
                    ToolContent::Text(text) | ToolContent::Output(text) => text.len(),
                    ToolContent::Diff(diff) => {
                        diff.path.len() + diff.old.as_ref().map_or(0, String::len) + diff.new.len()
                    }
                })
                .sum();
            let locations: usize = tool
                .locations
                .iter()
                .map(|location| location.path.len())
                .sum();
            tool.title.len() + content + locations
        }
    };
    let size = match patch {
        ChatPatch::Turn { turn, .. } => {
            turn.prompt.len() + turn.items.iter().map(item).sum::<usize>()
        }
        ChatPatch::Item { value, .. } => item(value),
        ChatPatch::Append { text, .. }
        | ChatPatch::Output { text, .. }
        | ChatPatch::Notice(Some(text)) => text.len(),
        ChatPatch::Question(Some(question)) => {
            question.title.len()
                + question
                    .choices
                    .iter()
                    .map(|choice| choice.name.len())
                    .sum::<usize>()
        }
        ChatPatch::Settings(settings) => (settings.iter())
            .flat_map(|setting| &setting.choices)
            .map(|choice| choice.name.len() + choice.value.len())
            .sum(),
        _ => 0,
    };
    // What each change takes, whatever's in it.
    size + 32
}

/// The chat's own setting for what its agent may do with clackatoo's
/// tools, at `tools`.
fn tools_setting(tools: AgentTools) -> Setting {
    Setting {
        id: TOOLS_SETTING.to_owned(),
        name: "Tools".to_owned(),
        kind: SettingKind::Tools,
        value: tools_value(tools).to_owned(),
        choices: (TOOLS_VALUES.iter())
            .map(|&(_, value, name, description)| SettingChoice {
                value: value.to_owned(),
                name: name.to_owned(),
                description: Some(description.to_owned()),
            })
            .collect(),
    }
}

/// How a chat keeps `tools` among the values the user gave settings.
fn tools_value(tools: AgentTools) -> &'static str {
    match tools {
        AgentTools::Off => "off",
        AgentTools::Ask => "ask",
        AgentTools::Allow => "allow",
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

/// Whether a tool of `kind` may change files: an edit, or a command, as
/// running a program is, which says nothing of what it changed. A read,
/// search, thought or fetch changes nothing.
fn writes_files(kind: ToolKind) -> bool {
    matches!(
        kind,
        ToolKind::Edit | ToolKind::Execute | ToolKind::Delete | ToolKind::Move | ToolKind::Other
    )
}

fn tool_kind(kind: agent_client_protocol_schema::v1::ToolKind) -> ToolKind {
    use agent_client_protocol_schema::v1::ToolKind as Acp;
    match kind {
        Acp::Read => ToolKind::Read,
        Acp::Edit => ToolKind::Edit,
        Acp::Delete => ToolKind::Delete,
        Acp::Move => ToolKind::Move,
        Acp::Search => ToolKind::Search,
        Acp::Execute => ToolKind::Execute,
        Acp::Think => ToolKind::Think,
        Acp::Fetch => ToolKind::Fetch,
        // Switching mode, or a kind newer than this build knows.
        _ => ToolKind::Other,
    }
}

/// What a tool shows, as the agent gives it: its text and its changes to
/// files, cut down to what a chat keeps. Terminals, which the daemon says
/// it doesn't run, and anything but text, are left out.
fn tool_content(content: &[ToolCallContent]) -> Vec<ToolContent> {
    (content.iter())
        .filter_map(|content| match content {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => {
                    Some(ToolContent::Text(tail(&clean_text(&text.text), TOOL_TEXT)))
                }
                _ => None,
            },
            ToolCallContent::Diff(diff) => Some(ToolContent::Diff(Diff {
                path: diff.path.to_string_lossy().into_owned(),
                old: (diff.old_text.as_deref()).map(|old| head(&clean_text(old), DIFF_TEXT)),
                new: head(&clean_text(&diff.new_text), DIFF_TEXT),
            })),
            _ => None,
        })
        .collect()
}

fn tool_locations(locations: &[ToolCallLocation]) -> Vec<Location> {
    (locations.iter())
        .map(|location| Location {
            path: location.path.to_string_lossy().into_owned(),
            line: location.line,
        })
        .collect()
}

/// The last whole lines of `text` that fit in `max` bytes, after "…", if
/// it's longer. A fence it opens with stays, for it to still read as code.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let fence = if text.starts_with("```") {
        text.split_inclusive('\n').next().unwrap_or_default()
    } else {
        ""
    };
    let mut start = text.len() - max.saturating_sub(fence.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    // Cut in a line: from the next.
    let rest = if text.as_bytes()[start - 1] == b'\n' {
        rest
    } else {
        rest.split_once('\n').map_or(rest, |(_, after)| after)
    };
    format!("{fence}…\n{rest}")
}

/// The first whole lines of `text` that fit in `max` bytes, if it's
/// longer.
fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    cut.rfind('\n').map_or(cut, |line| &cut[..=line]).to_owned()
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
            // Never handed over: the chat keeps its value with those chosen.
            SettingKind::Other | SettingKind::Tools => handover::SettingKind::Other,
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
                    kind: match tool.kind {
                        ToolKind::Read => handover::ToolKind::Read,
                        ToolKind::Edit => handover::ToolKind::Edit,
                        ToolKind::Delete => handover::ToolKind::Delete,
                        ToolKind::Move => handover::ToolKind::Move,
                        ToolKind::Search => handover::ToolKind::Search,
                        ToolKind::Execute => handover::ToolKind::Execute,
                        ToolKind::Think => handover::ToolKind::Think,
                        ToolKind::Fetch => handover::ToolKind::Fetch,
                        ToolKind::Other => handover::ToolKind::Other,
                    },
                    content: (tool.content.into_iter())
                        .map(|content| match content {
                            ToolContent::Text(text) => handover::ToolContent::Text(text),
                            ToolContent::Output(text) => handover::ToolContent::Output(text),
                            ToolContent::Diff(diff) => handover::ToolContent::Diff {
                                path: diff.path,
                                old: diff.old,
                                new: diff.new,
                            },
                        })
                        .collect(),
                    locations: (tool.locations.into_iter())
                        .map(|location| handover::ToolLocation {
                            path: location.path,
                            line: location.line,
                        })
                        .collect(),
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
                handover::ChatItem::Tool {
                    id,
                    title,
                    status,
                    kind,
                    content,
                    locations,
                } => Item::Tool(Tool {
                    id,
                    title,
                    status: match status {
                        handover::ToolStatus::Running => ToolStatus::Running,
                        handover::ToolStatus::Done => ToolStatus::Done,
                        handover::ToolStatus::Failed => ToolStatus::Failed,
                    },
                    kind: match kind {
                        handover::ToolKind::Read => ToolKind::Read,
                        handover::ToolKind::Edit => ToolKind::Edit,
                        handover::ToolKind::Delete => ToolKind::Delete,
                        handover::ToolKind::Move => ToolKind::Move,
                        handover::ToolKind::Search => ToolKind::Search,
                        handover::ToolKind::Execute => ToolKind::Execute,
                        handover::ToolKind::Think => ToolKind::Think,
                        handover::ToolKind::Fetch => ToolKind::Fetch,
                        handover::ToolKind::Other => ToolKind::Other,
                    },
                    content: (content.into_iter())
                        .map(|content| match content {
                            handover::ToolContent::Text(text) => ToolContent::Text(text),
                            handover::ToolContent::Output(text) => ToolContent::Output(text),
                            handover::ToolContent::Diff { path, old, new } => {
                                ToolContent::Diff(Diff { path, old, new })
                            }
                        })
                        .collect(),
                    locations: (locations.into_iter())
                        .map(|location| Location {
                            path: location.path,
                            line: location.line,
                        })
                        .collect(),
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
        files: None,
        // Put back from the chat's relations, if another agent sent it.
        from: None,
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol_schema::v1::{RequestPermissionRequest, SessionNotification};

    use super::*;

    fn chat() -> HostedChat {
        chat_with("claude")
    }

    /// A chat with the known agent called `name`.
    fn chat_with(name: &str) -> HostedChat {
        let agent = crate::agents::AgentConfig::default().find(name).unwrap();
        HostedChat::new(
            TerminalId(1),
            Size::new(40, 12).unwrap(),
            "/tmp".to_owned(),
            &agent,
            AgentTools::Ask,
            true,
        )
    }

    /// Answers the question `chat` shows with the choice `option`.
    fn answer(chat: &mut HostedChat, option: Option<String>) -> Vec<Action> {
        let shown = chat.node.question.as_ref();
        let question = shown.map_or(u64::MAX, |question| question.id);
        chat.act(ChatAction::Answer { question, option })
    }

    fn update(chat: &mut HostedChat, json: &str) -> Option<Wrote> {
        let json = format!(r#"{{"sessionId":"s","update":{json}}}"#);
        let notification: SessionNotification = serde_json::from_str(&json).unwrap();
        chat.update(notification.update)
    }

    #[test]
    fn a_finished_edit_says_what_it_wrote_and_whatever_is_running_is_known() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "edit it");
        let wrote = |paths: &[&str], diffs: Vec<RawDiff>| Wrote {
            done: true,
            turn: 0,
            tool: "e".into(),
            order: 0,
            paths: paths.iter().map(|&path| path.to_owned()).collect(),
            diffs,
        };
        // Asking first, as Claude Code's adapter does: nothing written yet,
        // but the file it's about to change is named.
        let pending = r#"{"sessionUpdate":"tool_call","toolCallId":"e","title":"Edit","kind":"edit",
                          "status":"pending","locations":[{"path":"/a"}]}"#;
        let about_to = Wrote {
            done: false,
            ..wrote(&["/a"], Vec::new())
        };
        assert_eq!(update(&mut chat, pending), Some(about_to));
        assert_eq!(chat.working_on(), (0, Some("e".to_owned())));
        // Finished, then what it wrote, each saying so.
        let done = r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","status":"completed"}"#;
        assert_eq!(update(&mut chat, done), Some(wrote(&["/a"], Vec::new())));
        assert_eq!(chat.working_on(), (0, None));
        let content = r#"{"sessionUpdate":"tool_call_update","toolCallId":"e",
                          "content":[{"type":"diff","path":"/b","oldText":"x","newText":"y"}],
                          "locations":[{"path":"/b","line":1},{"path":"/a"}]}"#;
        // What it says it changed, kept whole, from then on.
        let diffs = vec![RawDiff {
            path: "/b".into(),
            old: Some("x".into()),
            new: "y".into(),
        }];
        let both = wrote(&["/b", "/a"], diffs.clone());
        assert_eq!(update(&mut chat, content), Some(both));
        let done_again =
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","status":"completed"}"#;
        assert_eq!(
            update(&mut chat, done_again),
            Some(wrote(&["/b", "/a"], diffs))
        );
        // An update saying nothing of it says nothing again.
        let title = r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","title":"Edited"}"#;
        assert_eq!(update(&mut chat, title), None);
        // A command that finishes wrote nothing it says.
        let command = r#"{"sessionUpdate":"tool_call","toolCallId":"c","title":"ls",
                          "kind":"execute","status":"completed"}"#;
        assert_eq!(update(&mut chat, command), None);
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
                    kind: ToolKind::Other,
                    content: Vec::new(),
                    locations: Vec::new(),
                }),
                Item::Words("Done.".into()),
            ]
        );
        assert_eq!(outcome(turn), Some(&Outcome::Done));
    }

    #[test]
    fn a_tool_keeps_what_it_shows_which_each_update_replaces() {
        let mut chat = chat();
        chat.act(ChatAction::Send("run it".into()));
        let mut viewer = Viewer::new();
        viewer.catch_up(&chat);
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"b","title":"ls","kind":"execute","status":"pending",
                "content":[{"type":"content","content":{"type":"text","text":"List files"}}]}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"b","status":"completed",
                "content":[{"type":"content","content":{"type":"text","text":"```console\na\n```"}}]}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"w","title":"Write a","kind":"edit","status":"pending",
                "content":[{"type":"diff","path":"/src/a","oldText":null,"newText":"x\n"}]}"#,
        );
        let tools: Vec<(ToolKind, &[ToolContent])> = (chat.node.turns[0].items.iter())
            .filter_map(|item| match item {
                Item::Tool(tool) => Some((tool.kind, tool.content.as_slice())),
                _ => None,
            })
            .collect();
        assert_eq!(
            tools,
            [
                (
                    ToolKind::Execute,
                    &[ToolContent::Text("```console\na\n```".into())][..]
                ),
                (
                    ToolKind::Edit,
                    &[ToolContent::Diff(Diff {
                        path: "/src/a".into(),
                        old: None,
                        new: "x\n".into(),
                    })][..]
                ),
            ]
        );
        // Each change goes as the tool alone, not its turn.
        let Some(ViewUpdate::Patch(patches)) = viewer.catch_up(&chat) else {
            panic!("no patches");
        };
        assert!(
            (patches.iter()).all(|patch| matches!(patch, Patch::Chat(ChatPatch::Item { .. }))),
            "{patches:?}"
        );
    }

    #[test]
    fn a_long_output_keeps_its_end_and_a_long_diff_its_start() {
        let output = format!("```console\n{}```", "line\n".repeat(10));
        let kept = tail(&output, 24);
        assert_eq!(kept, "```console\n…\nline\nline\n```");
        assert_eq!(tail("short", 24), "short");
        assert_eq!(head(&"line\n".repeat(10), 12), "line\nline\n");
        // Never in the middle of a character.
        assert_eq!(tail("ééééé\nab", 3), "…\nab");
        assert_eq!(head("ééé", 3), "é");
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
            answer(&mut chat, Some("yes".into())),
            [Action::Answer {
                request: Value::from(7),
                option: Some("yes".into()),
            }]
        );
        assert_eq!(chat.node.question, None);
        // Nothing asked: an answer goes nowhere.
        assert_eq!(answer(&mut chat, None), []);
    }

    #[test]
    fn questions_wait_their_turn_and_tools_keep_theirs_past_the_turn() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let tool: ToolCallUpdate =
            serde_json::from_str(r#"{"toolCallId":"t","title":"Write a"}"#).unwrap();
        chat.ask(Value::from(1), &tool, &[]);
        chat.ask_for_tool(7, "run `make` in a terminal", None, Choices::Tool);
        chat.ask_for_tool(8, "run `ls` in a terminal", None, Choices::Tool);
        assert_eq!(chat.node.question.as_ref().unwrap().title, "Write a");
        assert_eq!(
            answer(&mut chat, Some("x".into())),
            [Action::Answer {
                request: Value::from(1),
                option: Some("x".into()),
            }]
        );
        let question = chat.node.question.clone().unwrap();
        assert_eq!(question.title, "run `make` in a terminal");
        assert_eq!(question.choices[1].kind, ChoiceKind::AllowAlways);
        // A call cancelled takes its question back.
        chat.withdraw(7);
        let title = &chat.node.question.as_ref().unwrap().title;
        assert_eq!(title, "run `ls` in a terminal");
        // The turn ending leaves it: the call still waits.
        chat.answered(Ok(StopReason::EndTurn));
        assert!(chat.node.question.is_some());
        assert_eq!(chat.tools(), AgentTools::Ask);
        assert_eq!(
            answer(&mut chat, Some("always".into())),
            [Action::Permit {
                call: 8,
                allow: true,
                pair: false,
            }]
        );
        assert_eq!(
            chat.tools(),
            AgentTools::Allow,
            "always allowed from then on"
        );
        assert_eq!(chat.node.question, None);
        // Kept through an upgrade, and never sent to the agent.
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let mut resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert_eq!(resumed.tools(), AgentTools::Allow);
        let opened = Opened {
            session: String::new(),
            settings: None,
        };
        assert_eq!(resumed.connected(opened), []);
    }

    #[test]
    fn codexs_question_about_clackatoos_own_tools_is_answered_at_once() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"m","title":"mcp.clackatoo.run_in_terminal",
                "kind":"execute","status":"pending","rawInput":{"server":"clackatoo"}}"#,
        );
        let request: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"m","kind":"execute"},
                "options":[{"optionId":"approved","name":"Allow","kind":"allow_once"},
                           {"optionId":"cancel","name":"Cancel","kind":"reject_once"}]}"#,
        )
        .unwrap();
        let allowed = chat.about_own_tools(&request.tool_call, &request.options);
        assert_eq!(allowed.as_deref(), Some("approved"));
        // Another server's tool, or a command, is the user's to answer.
        let theirs: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"x","title":"mcp.other.run",
                "rawInput":{"serverName":"other"}},
                "options":[{"optionId":"approved","name":"Allow","kind":"allow_once"}]}"#,
        )
        .unwrap();
        assert_eq!(
            chat.about_own_tools(&theirs.tool_call, &theirs.options),
            None
        );
    }

    #[test]
    fn an_answer_to_a_question_taken_back_lands_on_no_other() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        chat.ask_for_tool(1, "run `a` in a new terminal", None, Choices::Tool);
        chat.ask_for_tool(2, "run `b` in a new terminal", None, Choices::Tool);
        let shown = chat.node.question.clone().unwrap();
        // The call is cancelled as the user answers it.
        chat.withdraw(1);
        let stale = ChatAction::Answer {
            question: shown.id,
            option: Some("allow".into()),
        };
        assert_eq!(chat.act(stale.clone()), []);
        let next = chat.node.question.clone().unwrap();
        assert_eq!(next.title, "run `b` in a new terminal");
        // Answered once, a second press of the same answer does nothing.
        let once = ChatAction::Answer {
            question: next.id,
            option: Some("allow".into()),
        };
        assert_eq!(
            chat.act(once.clone()),
            [Action::Permit {
                call: 2,
                allow: true,
                pair: false,
            }]
        );
        assert_eq!(chat.act(once), []);
    }

    #[test]
    fn the_tools_setting_is_the_chats_own_and_never_the_agents() {
        let mut chat = chat();
        connect(&mut chat, "s");
        let set = |value: &str| ChatAction::Set {
            setting: TOOLS_SETTING.into(),
            value: value.into(),
        };
        assert_eq!(chat.act(set("allow")), [], "the agent never hears of it");
        assert_eq!(chat.tools(), AgentTools::Allow);
        let shown = (chat.node.settings.iter()).find(|setting| setting.id == TOOLS_SETTING);
        assert_eq!(shown.map(|setting| setting.value.as_str()), Some("allow"));
        assert_eq!(chat.act(set("ask")), []);
        assert_eq!(chat.tools(), AgentTools::Ask);
        assert_eq!(chat.act(set("everything")), []);
        assert_eq!(chat.tools(), AgentTools::Ask);
        // Kept when the agent's settings change.
        chat.settings_changed(Ok(claude_settings("sonnet")));
        assert_eq!(chat.node.settings.last().unwrap().id, TOOLS_SETTING);
    }

    #[test]
    fn too_many_questions_are_refused_and_long_text_is_shown_whole() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let whole = "x".repeat(5000);
        assert!(chat.ask_for_tool(
            0,
            "type 5 KB of text into pane 2",
            Some(&whole),
            Choices::Tool
        ));
        let card = (chat.node.turns[0].items.iter()).find_map(|item| match item {
            Item::Tool(tool) => Some(tool.content.clone()),
            _ => None,
        });
        assert_eq!(
            card,
            Some(vec![ToolContent::Text(format!("```\n{whole}\n```"))])
        );
        for call in 1..MAX_QUESTIONS as u64 {
            assert!(chat.ask_for_tool(call, "run `x` in a new terminal", None, Choices::Tool));
        }
        assert!(!chat.ask_for_tool(99, "run `y` in a new terminal", None, Choices::Tool));
    }

    #[test]
    fn stopping_refuses_the_tools_questions_too() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "go");
        chat.ask_for_tool(3, "type `y` into pane 2", None, Choices::Tool);
        assert_eq!(
            chat.act(ChatAction::Stop),
            [
                Action::Permit {
                    call: 3,
                    allow: false,
                    pair: false,
                },
                Action::Cancel
            ]
        );
        assert_eq!(chat.node.question, None);
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
        chat.disconnected(None);
        let turn = &chat.node.turns[0];
        assert_eq!(
            outcome(turn),
            Some(&Outcome::Failed("the agent stopped".into()))
        );
        assert!(matches!(&turn.items[0], Item::Tool(tool) if tool.status == ToolStatus::Failed));
        // Idle, it says so, and the next message opens the session again.
        connect(&mut chat, "");
        chat.disconnected(None);
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
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert_eq!(resumed.session(), Some("s"));
        let turn = &resumed.node.turns[0];
        assert_eq!(turn.items, [Item::Words("hello".into())]);
        assert_eq!(
            outcome(turn),
            Some(&Outcome::Failed("cut off by an upgrade".into()))
        );
        assert_eq!(resumed.node.state, ChatState::Ready);
    }

    #[test]
    fn a_chat_without_claude_codes_task_tool_stays_without_it() {
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let size = Size::new(40, 12).unwrap();
        let tools = AgentTools::Ask;
        let mut chat = HostedChat::new(TerminalId(1), size, "/".into(), &agent, tools, false);
        assert!(!chat.task());
        // It's never shown, and never sent to the agent as a setting.
        let actions = connect(&mut chat, "s");
        assert!(
            !(chat.node.settings.iter()).any(|setting| setting.id == TASK_SETTING),
            "{:?}",
            chat.node.settings
        );
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::Set { .. })),
            "{actions:?}"
        );
        // Nor can a window change it once the session is open.
        chat.act(ChatAction::Set {
            setting: TASK_SETTING.into(),
            value: "on".into(),
        });
        assert!(!chat.task());
        // It lasts through an upgrade, so the session opens again without it.
        let resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert!(!resumed.task());
        // A chat from before it could be taken away keeps it.
        let mut handed = resumed.hand_over();
        handed.chosen.retain(|(id, _)| id != TASK_SETTING);
        assert!(HostedChat::resume(handed, &agent).task());
        assert!(chat_with("claude").task());
    }

    /// A message from chat `from`, `hops` along a chain.
    fn message(from: u32, text: &str, hops: u8) -> Message {
        Message {
            from: Sender {
                chat: TerminalId(from),
                agent: "Codex".into(),
                title: "fixes".into(),
            },
            text: text.into(),
            hops,
            cause: None,
        }
    }

    #[test]
    fn a_message_from_another_agent_waits_for_the_turn_then_goes_marked() {
        let mut chat = chat();
        connect(&mut chat, "s");
        // Idle, it goes at once, as a turn marked as from the sender, the
        // agent told who sent it.
        let actions = chat.deliver(message(7, "hi", 1)).unwrap();
        let [Action::Prompt(prompt)] = actions.as_slice() else {
            panic!("{actions:?}");
        };
        assert!(
            prompt.starts_with("[Message from chat 7 (Codex, \"fixes\"): another agent"),
            "{prompt}"
        );
        assert!(
            prompt.contains(" from chat 7 ---\nhi\n--- end of message "),
            "{prompt}"
        );
        let turn = &chat.node.turns[0];
        assert_eq!(turn.prompt, "hi");
        assert_eq!(turn.from.as_ref().unwrap().chat, TerminalId(7));
        assert_eq!(chat.relations().hops, 1);
        // Answering, the next waits, up to the most kept.
        for n in 0..MAX_INBOX {
            assert_eq!(chat.deliver(message(7, &format!("{n}"), 1)), Ok(Vec::new()));
        }
        assert!(chat.deliver(message(7, "one too many", 1)).is_err());
        assert_eq!(chat.deliver_next(), [], "not while answering");
        chat.answered(Ok(StopReason::EndTurn));
        let next = chat.deliver_next();
        assert!(
            matches!(next.as_slice(), [Action::Prompt(prompt)] if prompt.contains(" ---\n0\n--- end"))
        );
        assert_eq!(chat.relations().inbox.len(), MAX_INBOX - 1);
        // The user's own message starts the chain again.
        chat.answered(Ok(StopReason::EndTurn));
        chat.relations.inbox.clear();
        send(&mut chat, "mine");
        assert_eq!(chat.relations().hops, 0);
        assert_eq!(chat.node.turns.last().unwrap().from, None);
    }

    #[test]
    fn always_allowing_a_message_is_for_that_chat_and_never_the_tools_setting() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "work");
        assert!(chat.ask_for_tool(4, "send `hi` to chat 2", None, Choices::Talk));
        let question = chat.node.question.clone().unwrap();
        let names: Vec<&str> = (question.choices.iter())
            .map(|choice| choice.name.as_str())
            .collect();
        assert_eq!(names, ["Allow", "Always allow for that chat", "Deny"]);
        assert_eq!(
            answer(&mut chat, Some("pair".into())),
            [Action::Permit {
                call: 4,
                allow: true,
                pair: true,
            }]
        );
        assert_eq!(chat.tools(), AgentTools::Ask, "the setting is as it was");
    }

    #[test]
    fn stopping_drops_the_messages_waiting_and_says_so() {
        let mut chat = chat();
        connect(&mut chat, "s");
        send(&mut chat, "work");
        chat.deliver(message(7, "later", 1)).unwrap();
        assert_eq!(chat.act(ChatAction::Stop), [Action::Cancel]);
        assert!(chat.relations().inbox.is_empty());
        assert_eq!(
            chat.node.notice.as_deref(),
            Some("Stopped: 1 message from other agents waiting their turn weren't sent.")
        );
    }

    #[test]
    fn where_a_chat_stands_among_the_others_survives_an_upgrade() {
        let mut chat = chat();
        connect(&mut chat, "s");
        chat.started_by(TerminalId(3), TerminalId(2), 1, None);
        chat.deliver(message(3, "do it", 2)).unwrap();
        chat.answered(Ok(StopReason::EndTurn));
        send(&mut chat, "and you");
        chat.deliver(message(5, "waiting", 3)).unwrap();
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let resumed = HostedChat::resume(chat.hand_over(), &agent);
        assert_eq!(resumed.relations().parent, Some(TerminalId(3)));
        assert_eq!(resumed.relations().depth, 1);
        assert_eq!(resumed.relations().inbox, [message(5, "waiting", 3)]);
        let senders: Vec<Option<TerminalId>> = (resumed.node.turns.iter())
            .map(|turn| turn.from.as_ref().map(|from| from.chat))
            .collect();
        assert_eq!(senders, [Some(TerminalId(3)), None]);
        // It isn't one of the values the agent is asked for.
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
        // The chat's own setting for clackatoo's tools comes after the
        // agent's.
        assert_eq!(
            modes,
            [
                (SettingKind::Mode, "auto"),
                (SettingKind::Model, "haiku"),
                (SettingKind::Tools, "ask")
            ]
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
        chat.disconnected(None);
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

    #[test]
    fn a_chat_is_named_after_its_agent_and_starts_in_the_agents_own_mode() {
        let chat = chat_with("codex");
        assert_eq!(
            (chat.title.as_str(), chat.node.agent.as_str()),
            ("Codex", "Codex")
        );
        assert_eq!(chat.agent(), "codex");
        assert_eq!(chat.starting_mode, None);
        assert_eq!(chat_with("claude").starting_mode.as_deref(), Some("auto"));
    }

    /// Has `chat`'s agent stream `data` as the output of tool `e`.
    fn output(chat: &mut HostedChat, data: &str) {
        let data = serde_json::to_string(data).unwrap();
        update(
            chat,
            &format!(
                r#"{{"sessionUpdate":"tool_call_update","toolCallId":"e",
                    "_meta":{{"terminal_output_delta":{{"data":{data},"terminal_id":"e"}}}}}}"#
            ),
        );
    }

    /// Starts tool `e`, a command, as Codex's adapter does.
    fn start_command(chat: &mut HostedChat) {
        update(
            chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"e","title":"cat x","kind":"execute",
                "status":"in_progress","content":[{"type":"terminal","terminalId":"e"}]}"#,
        );
    }

    #[test]
    fn streamed_output_goes_as_appends_and_is_cut_to_its_end() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        start_command(&mut chat);
        let shown = |chat: &HostedChat| match &chat.node.turns[0].items[0] {
            Item::Tool(tool) => tool.content.clone(),
            other => panic!("{other:?}"),
        };
        // Nothing shown of a terminal the daemon never made.
        assert_eq!(shown(&chat), []);
        let before = chat.journal.end();
        output(&mut chat, "a ``` b\n");
        assert_eq!(shown(&chat), [ToolContent::Output("a ``` b\n".into())]);
        assert_eq!(
            chat.journal.patches.back(),
            Some(&ChatPatch::Output {
                turn: 0,
                item: 0,
                text: "a ``` b\n".into(),
            })
        );
        assert_eq!(chat.journal.end(), before + 1, "the piece alone");
        let line = "x".repeat(99) + "\n";
        for _ in 0..1000 {
            output(&mut chat, &line);
        }
        let content = shown(&chat);
        let [ToolContent::Output(text)] = content.as_slice() else {
            panic!()
        };
        assert!(text.len() <= 2 * TOOL_TEXT, "{}", text.len());
        assert!(
            text.starts_with("…\nxxx") && text.ends_with("x\n"),
            "{text}"
        );
    }

    #[test]
    fn a_flood_of_output_keeps_the_journal_bounded_and_a_late_viewer_gets_it_once() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        let mut late = Viewer::new();
        late.catch_up(&chat);
        let mut keeping_up = Viewer::new();
        keeping_up.catch_up(&chat);
        send(&mut chat, "build");
        start_command(&mut chat);
        let line = "y".repeat(199) + "\n";
        for at in 0..10_000 {
            output(&mut chat, &line);
            if at % 1000 == 0 {
                keeping_up.catch_up(&chat);
            }
        }
        assert!(
            chat.journal.bytes <= JOURNAL_BYTES,
            "{}",
            chat.journal.bytes
        );
        assert!(chat.journal.patches.len() > 1);
        // Behind by the whole flood: one copy of the output, not one for
        // each piece, however it comes.
        let update = late.catch_up(&chat).unwrap();
        let size = postcard::to_allocvec(&update).unwrap().len();
        assert!(size < 4 * TOOL_TEXT, "{size}");
        keeping_up.catch_up(&chat);
        for viewer in [&late, &keeping_up] {
            assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
        }
    }

    #[test]
    fn a_later_value_of_an_item_takes_the_place_of_the_first_in_an_update() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        let mut viewer = Viewer::new();
        viewer.catch_up(&chat);
        start_command(&mut chat);
        output(&mut chat, "one\n");
        // Another tool after it, then the first again, finished.
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"f","title":"ls","kind":"execute",
                "status":"in_progress"}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"e","status":"completed"}"#,
        );
        output(&mut chat, "two\n");
        let Some(ViewUpdate::Patch(patches)) = viewer.catch_up(&chat) else {
            panic!()
        };
        let items: Vec<(u32, bool)> = (patches.iter())
            .filter_map(|patch| match patch {
                Patch::Chat(ChatPatch::Item { item, .. }) => Some((*item, false)),
                Patch::Chat(ChatPatch::Output { item, .. }) => Some((*item, true)),
                _ => None,
            })
            .collect();
        // The first tool once, where it was first added, with the output
        // it had by then; the second; then the output since.
        assert_eq!(items, [(0, false), (1, false), (0, true)]);
        assert_eq!(viewer.view.as_ref(), Some(&chat.view()));
    }

    #[test]
    fn a_question_says_what_the_tool_said_it_does() {
        let mut chat = chat_with("codex");
        connect(&mut chat, "s");
        send(&mut chat, "go");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t","title":"rm -r build","kind":"execute",
                "status":"pending"}"#,
        );
        let request: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"t","title":"Run command"},
                "options":[{"optionId":"a","name":"Yes","kind":"allow_once"}]}"#,
        )
        .unwrap();
        chat.ask(Value::from(1), &request.tool_call, &request.options);
        assert_eq!(chat.node.question.as_ref().unwrap().title, "rm -r build");
        answer(&mut chat, Some("a".into()));
        // One it hasn't said anything of goes by the question's.
        let request: RequestPermissionRequest = serde_json::from_str(
            r#"{"sessionId":"s","toolCall":{"toolCallId":"u","title":"Write a"},"options":[]}"#,
        )
        .unwrap();
        chat.ask(Value::from(2), &request.tool_call, &request.options);
        assert_eq!(chat.node.question.as_ref().unwrap().title, "Write a");
    }

    #[test]
    fn an_install_offer_holds_the_message_until_its_answered() {
        let mut chat = chat_with("codex");
        chat.offer_install("Codex isn't installed. Install it?");
        assert!(chat.offering_install());
        assert_eq!(send(&mut chat, "hi"), []);
        assert_eq!(answer(&mut chat, Some(INSTALL.into())), [Action::Install]);
        assert_eq!(chat.node.question, None);
        assert_eq!(chat.node.notice.as_deref(), Some("Installing Codex…"));
        assert_eq!(chat.node.state, ChatState::Starting);
        // Installed, its session opens and the message goes.
        chat.installed();
        assert_eq!(chat.node.notice, None);
        assert_eq!(connect(&mut chat, "s"), [Action::Prompt("hi".into())]);

        // No: the message goes nowhere, and the next asks again.
        let mut chat = chat_with("codex");
        chat.offer_install("Install?");
        send(&mut chat, "hi");
        assert_eq!(answer(&mut chat, Some("later".into())), []);
        assert!(
            matches!(&chat.node.state, ChatState::Failed(why) if why.contains("isn't installed"))
        );
        assert!(matches!(
            outcome(&chat.node.turns[0]),
            Some(Outcome::Failed(_))
        ));
        assert_eq!(send(&mut chat, "again"), [Action::Connect]);

        // Stopped while it's asked: the message is taken back.
        let mut chat = chat_with("codex");
        chat.offer_install("Install?");
        send(&mut chat, "hi");
        assert_eq!(chat.act(ChatAction::Stop), []);
        assert_eq!(outcome(&chat.node.turns[0]), Some(&Outcome::Stopped));
        assert_eq!(
            (chat.node.question.as_ref(), chat.offering_install()),
            (None, false)
        );
    }

    #[test]
    fn a_long_title_is_cut_short() {
        let mut chat = chat_with("codex");
        let title = "é".repeat(100);
        update(
            &mut chat,
            &format!(r#"{{"sessionUpdate":"session_info_update","title":"{title}"}}"#),
        );
        assert_eq!(chat.title, "é".repeat(TITLE) + "…");
    }

    #[test]
    fn an_agent_that_never_started_says_why() {
        let mut chat = chat_with("codex");
        send(&mut chat, "hi");
        chat.disconnected(Some("npm ERR! nope"));
        assert_eq!(
            chat.node.state,
            ChatState::Failed("it stopped, saying: npm ERR! nope".into())
        );
        assert_eq!(
            outcome(&chat.node.turns[0]),
            Some(&Outcome::Failed(
                "Codex couldn't start: it stopped, saying: npm ERR! nope".into()
            ))
        );
    }
}
