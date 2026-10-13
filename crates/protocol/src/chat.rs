//! Chats with agents: what a chat's view holds, the patches that keep it
//! current, and what a client can ask of a chat.
//!
//! A chat is a list of turns: each a message the user sent, and what the
//! agent did with it, in order: its words, its thinking and the tools it
//! used. The agent's words are Markdown, which clients lay out for
//! themselves, at whatever width they have.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// A chat with an agent, as a view's node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatNode {
    /// The agent the chat talks to, as it's shown: "Claude Code",
    /// "Codex"…
    pub agent: String,
    /// The turns, oldest first.
    pub turns: Vec<Turn>,
    /// What the chat is doing.
    pub state: ChatState,
    /// What the agent is asking, waiting for an answer.
    pub question: Option<Question>,
    /// Something to tell the user that belongs to no turn, such as that
    /// the agent stopped while the chat was idle.
    pub notice: Option<String>,
    /// The agent's settings for the chat, such as its mode and model, as
    /// the agent offers them.
    pub settings: Vec<Setting>,
    /// How much of the agent's context the chat fills, once it has said.
    pub usage: Option<Usage>,
    /// The other agents the chat could move to, and their models, for
    /// the model menu to offer beside its own agent's.
    pub providers: Vec<Provider>,
    /// The chats its agent started, oldest first: clients show them as a
    /// card in the turn that started them, and open one as a pane.
    pub subagents: Vec<Subagent>,
}

/// A chat another chat's agent started, as the chat that started it shows
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    /// The chat.
    pub chat: crate::TerminalId,
    /// Its agent, as it's shown: "Claude Code".
    pub agent: String,
    /// The profile it was started with, such as `explore`, if one.
    pub profile: Option<String>,
    /// The model it runs on, as shown, once that's known.
    pub model: Option<String>,
    /// What it was asked to do: its first message's first line, cut short.
    pub task: String,
    /// How it's going.
    pub status: SubagentStatus,
    /// When it was started, in milliseconds since the Unix epoch.
    pub started: u64,
    /// When it last finished, if it isn't working now.
    pub ended: Option<u64>,
    /// The turn of the chat that started it that it was started in, by
    /// its index: the card goes there.
    pub turn: u32,
    /// Whether it's in a pane of its own, rather than in the background.
    pub shown: bool,
}

/// How a chat another chat's agent started is going.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubagentStatus {
    /// Its agent is starting.
    Starting,
    /// Its agent is answering.
    Working,
    /// Its agent asks the user something, and waits: it must be opened to
    /// answer.
    Asking,
    /// It finished, and waits for another message.
    Done,
    /// Its last turn went wrong, or its agent couldn't start: why.
    Failed(String),
    /// It was closed.
    Closed,
}

impl SubagentStatus {
    /// Whether it's still going: starting, working or asking.
    #[must_use]
    pub fn going(&self) -> bool {
        matches!(self, Self::Starting | Self::Working | Self::Asking)
    }
}

/// Another agent a chat could move to, and the models it offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Its name, to switch to it by: `codex`.
    pub agent: String,
    /// What to show: "Codex".
    pub title: String,
    /// Its models, as far as they're known.
    pub models: ProviderModels,
}

/// What's known of another agent's models.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderModels {
    /// Nothing yet: opening the model menu asks the daemon to find out.
    Unknown,
    /// The daemon is asking the agent.
    Learning,
    /// These, as the agent offers them.
    Known(Vec<SettingChoice>),
    /// It isn't installed: picking it offers to install it.
    NotInstalled,
    /// The agent couldn't say: why. Picking it switches anyway, and the
    /// chat says what's wrong.
    Failed(String),
}

/// One of the agent's settings for a chat, and the values it can take.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    /// What the agent calls it, to change it with.
    pub id: String,
    /// What to show: "Mode", "Model"…
    pub name: String,
    /// What it's about.
    pub kind: SettingKind,
    /// The value it has, by its ID.
    pub value: String,
    /// The values it can take.
    pub choices: Vec<SettingChoice>,
}

/// What a setting is about, so clients know where to put it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingKind {
    /// How much the agent may do without asking, such as Claude Code's
    /// Manual, Accept edits, Plan and Auto.
    Mode,
    /// The model it runs on.
    Model,
    /// How hard it thinks.
    Effort,
    /// What the agent may do with clackatoo's own tools: a setting of
    /// the daemon's, not the agent's.
    Tools,
    /// Anything else.
    Other,
    /// Whether the agent gets tools that drive clackatoo's browser: a
    /// setting of the daemon's, not the agent's.
    Browser,
}

/// One value a setting can take.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChoice {
    /// What the agent calls it.
    pub value: String,
    /// What to show: "Auto", "Sonnet 5.5"…
    pub name: String,
    /// More about it, if the agent says: "Fastest for quick answers".
    pub description: Option<String>,
}

/// How much of the agent's context a chat fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens in the context.
    pub used: u64,
    /// The most it holds.
    pub size: u64,
}

impl Setting {
    /// The choice it has now, if it's one of its choices.
    #[must_use]
    pub fn current(&self) -> Option<&SettingChoice> {
        self.choices
            .iter()
            .find(|choice| choice.value == self.value)
    }
}

/// A message the user sent, and what the agent did with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// The message. Empty for a turn holding a conversation from before
    /// chats were kept as turns.
    pub prompt: String,
    /// When it was sent, in milliseconds since the Unix epoch.
    pub sent: u64,
    /// What the agent did, in order.
    pub items: Vec<Item>,
    /// How the turn ended, once it has.
    pub end: Option<TurnEnd>,
    /// What the agent changed in files in the turn, if it changed any.
    pub files: Option<TurnFiles>,
    /// The chat whose agent sent the message, if another agent sent it
    /// rather than the user: clients show it beside the message.
    pub from: Option<Sender>,
    /// Where the chat moved to another agent: a turn of its own, with no
    /// message and nothing done, that's shown as a line across the chat.
    pub switch: Option<Switch>,
}

/// The chat whose agent sent a message to another chat, as it was when it
/// sent it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// The chat.
    pub chat: crate::TerminalId,
    /// Its agent, as it's shown: "Claude Code", "Codex"…
    pub agent: String,
    /// Its title.
    pub title: String,
}

/// A chat moving to another agent, or back to one it had before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switch {
    /// The agent it moved from, as shown: "Claude Code".
    pub from: String,
    /// The agent it moved to, as shown: "Codex".
    pub to: String,
    /// The model it runs on there, as shown, once that's known.
    pub model: Option<String>,
    /// Whether it went back to a session it had with that agent before,
    /// which hears only what happened since, rather than starting one that
    /// hears the conversation so far.
    pub resumed: bool,
    /// Settings that didn't carry over, as "Mode Auto isn't Codex's: it
    /// uses its own default".
    pub notes: Vec<String>,
}

impl Switch {
    /// What the line across the chat says: "Switched to Codex · 6 Luna ·
    /// context carried over".
    #[must_use]
    pub fn label(&self) -> String {
        let mut label = if self.resumed {
            format!("Switched back to {}", self.to)
        } else {
            format!("Switched to {}", self.to)
        };
        if let Some(model) = &self.model {
            let _ = write!(label, " · {model}");
        }
        label.push_str(if self.resumed {
            " · caught up on what happened since"
        } else {
            " · context carried over"
        });
        label
    }
}

/// What an agent changed in files in a turn, and whether it can be undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFiles {
    /// How many files it changed, made or deleted.
    pub count: u32,
    /// Where they are.
    pub state: TurnFilesState,
}

/// Whether a turn's changes to files are made, or undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnFilesState {
    /// Made, as the agent made them: they can be undone.
    Made,
    /// Undone: they can be made again.
    Undone,
    /// Undone in some files, as forced where others couldn't be.
    Partly,
    /// Too old: the daemon no longer keeps what they were, so they can't
    /// be undone.
    TooOld,
}

/// How a turn ended, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEnd {
    /// When, in milliseconds since the Unix epoch.
    pub at: u64,
    /// How.
    pub outcome: Outcome,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// The agent finished.
    Done,
    /// The user stopped it.
    Stopped,
    /// It went wrong: why, to show the user.
    Failed(String),
}

/// One thing the agent did in a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Item {
    /// Its words, in Markdown.
    Words(String),
    /// Its thinking, in Markdown.
    Thought(String),
    /// A tool it used.
    Tool(Tool),
}

/// A tool the agent used, such as reading a file or running a command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// What the agent calls it, to find it again.
    pub id: String,
    /// What it does, as the agent describes it: "Read Cargo.toml".
    pub title: String,
    /// How it's going.
    pub status: ToolStatus,
    /// What sort of tool it is.
    pub kind: ToolKind,
    /// What it shows: what it's for, its output, or the changes it makes
    /// to files.
    pub content: Vec<ToolContent>,
    /// Where in files it works, as the agent says: a change to a file is
    /// at the line its location gives.
    pub locations: Vec<Location>,
}

/// A file a tool works in, and where in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// The file's path.
    pub path: String,
    /// The line, from 1, if the agent says.
    pub line: Option<u32>,
}

/// What sort of tool an agent used, as it says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    /// Reads files.
    Read,
    /// Changes files.
    Edit,
    /// Deletes files.
    Delete,
    /// Moves or renames files.
    Move,
    /// Searches files.
    Search,
    /// Runs a command: its title is the command.
    Execute,
    /// Thinks.
    Think,
    /// Fetches from the web.
    Fetch,
    /// Something else.
    #[default]
    Other,
}

/// Something a tool shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolContent {
    /// Text, in Markdown: a command's output comes fenced, as code.
    Text(String),
    /// A change to a file.
    Diff(Diff),
    /// A command's output as it ran, as plain text, streamed apart from
    /// the tool's other content, as Codex's adapter streams it.
    Output(String),
}

impl Tool {
    /// Adds `text` to the end of its output.
    pub fn append_output(&mut self, text: &str) {
        match self.output_mut() {
            Some(output) => output.push_str(text),
            None => self.content.push(ToolContent::Output(text.to_owned())),
        }
    }

    /// Its output, if it has any.
    pub fn output_mut(&mut self) -> Option<&mut String> {
        self.content.iter_mut().find_map(|content| match content {
            ToolContent::Output(output) => Some(output),
            _ => None,
        })
    }
}

/// A change to a file: the part of it changed, before and after.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// The file's path.
    pub path: String,
    /// What was there, or `None` for a new file.
    pub old: Option<String>,
    /// What's there now.
    pub new: String,
}

/// How a tool call is going.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    /// Still running.
    Running,
    /// Finished.
    Done,
    /// Failed.
    Failed,
}

/// What a chat is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatState {
    /// The agent is starting, or opening the chat's session.
    #[default]
    Starting,
    /// Waiting for a message.
    Ready,
    /// The agent is answering.
    Answering,
    /// The agent couldn't start: why. The next message tries again.
    Failed(String),
}

/// The agent asking whether it may use a tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// Its number, which an answer gives back, so an answer meant for one
    /// question never lands on the next.
    pub id: u64,
    /// What it wants to do, as the agent describes it: "Write notes.txt".
    pub title: String,
    /// The answers it offers.
    pub choices: Vec<Choice>,
}

/// One answer to a question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// What the agent calls it, to answer with.
    pub id: String,
    /// What to show: "Allow", "Always allow"…
    pub name: String,
    /// What it means.
    pub kind: ChoiceKind,
}

/// What an answer means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChoiceKind {
    /// Yes, this once.
    AllowOnce,
    /// Yes, and don't ask again.
    AllowAlways,
    /// No, this once.
    RejectOnce,
    /// No, and don't ask again.
    RejectAlways,
}

/// A chat as the list of chats shows it: enough to say which need the
/// user, without each one's turns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSummary {
    /// The chat.
    pub chat: crate::TerminalId,
    /// Its title.
    pub title: String,
    /// Its agent, as it's shown: "Claude Code".
    pub agent: String,
    /// What it's doing.
    pub state: ChatState,
    /// Whether its agent asks the user something, and waits.
    pub asking: bool,
    /// Whether a turn ended, or a question came, while no one present was
    /// looking at it, and no one has looked since.
    pub unseen: bool,
    /// The chat whose agent started it, if one did.
    pub parent: Option<crate::TerminalId>,
    /// The workspace it's in: its pane's, or for a chat in no pane, that
    /// of the nearest chat above it that's in one.
    pub workspace: Option<crate::WorkspaceId>,
    /// When something last happened in it, in milliseconds since the Unix
    /// epoch: its last message sent, or its last turn's end.
    pub active: u64,
}

impl ChatSummary {
    /// Whether it needs the user: it asks something, or has something
    /// they haven't seen.
    #[must_use]
    pub fn needs_you(&self) -> bool {
        self.asking || self.unseen
    }
}

/// One change to a chat a client already has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatPatch {
    /// The turn at `index` is now `turn`. An index one past the last adds
    /// a turn.
    Turn {
        /// Which turn.
        index: u32,
        /// What it is now.
        turn: Turn,
    },
    /// Item `item` of turn `turn` is now `value`, as a tool is as it
    /// goes. An index one past the last adds an item.
    Item {
        /// The turn's index.
        turn: u32,
        /// The item's index in the turn.
        item: u32,
        /// What it is now.
        value: Item,
    },
    /// More of the agent's words, or its thinking: `text` goes on the end
    /// of item `item` of turn `turn`.
    Append {
        /// The turn's index.
        turn: u32,
        /// The item's index in the turn.
        item: u32,
        /// What's added.
        text: String,
    },
    /// More of a command's output: `text` goes on the end of the output
    /// of the tool that's item `item` of turn `turn`.
    Output {
        /// The turn's index.
        turn: u32,
        /// The tool's index in the turn.
        item: u32,
        /// What's added.
        text: String,
    },
    /// The chat's state is now this.
    State(ChatState),
    /// The agent's question is now this.
    Question(Option<Question>),
    /// The notice is now this.
    Notice(Option<String>),
    /// The settings are now these.
    Settings(Vec<Setting>),
    /// The usage is now this.
    Usage(Option<Usage>),
    /// What turn `turn` changed in files is now `files`.
    Files {
        /// The turn's index.
        turn: u32,
        /// What it changed.
        files: Option<TurnFiles>,
    },
    /// The chat talks to this agent now, as it's shown.
    Agent(String),
    /// The other agents it could move to are now these.
    Providers(Vec<Provider>),
    /// The chats its agent started are now these.
    Subagents(Vec<Subagent>),
}

/// What a client asks of a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatAction {
    /// Send this message, starting a turn.
    Send(String),
    /// Stop the agent answering.
    Stop,
    /// Answer the agent's question numbered `question` with the choice
    /// with this ID, or with none. An answer to a question no longer
    /// asked is ignored.
    Answer {
        /// The question's number.
        question: u64,
        /// The choice's ID.
        option: Option<String>,
    },
    /// Change one of the agent's settings for the chat.
    Set {
        /// The setting's ID.
        setting: String,
        /// The value to give it, by its ID.
        value: String,
    },
    /// Undo what the agent changed in files in turn `turn`, keeping what
    /// anyone changed since. If any file can't be, because a change since
    /// touches the same lines, none is, unless `force`, which undoes the
    /// rest.
    Undo {
        /// The turn's index.
        turn: u32,
        /// Undo what can be, though some files can't be.
        force: bool,
    },
    /// Make again what undoing turn `turn` took out of files, as `Undo`.
    Redo {
        /// The turn's index.
        turn: u32,
        /// Redo what can be, though some files can't be.
        force: bool,
    },
    /// Move the chat to another agent, on `model`, or the agent's own
    /// choice: a session with it hears the conversation so far, or what
    /// happened since, if it had one before, with its next message. Not
    /// while the agent is answering.
    Switch {
        /// The agent, by its name.
        agent: String,
        /// The model, by its value, as the agent offers it.
        model: Option<String>,
        /// About the most of the conversation it hears, in tokens.
        budget: u32,
    },
    /// Find out the models of the other agents the chat could move to,
    /// where they aren't known, as the model menu opens.
    LearnModels,
    /// Show `chat`, one this chat's agent started, in a pane beside this
    /// chat, or point it out if it's in one already.
    Open {
        /// The chat to show.
        chat: crate::TerminalId,
    },
}

/// The number turn `index` of `turns` goes by, as people count them: from
/// 1, counting only turns with a message, not those where the chat moved
/// to another agent. `None` for one of those, or past the end.
#[must_use]
pub fn turn_number(turns: &[Turn], index: usize) -> Option<usize> {
    let turn = turns.get(index)?;
    let before = turns[..index].iter().filter(|turn| turn.switch.is_none());
    turn.switch.is_none().then(|| before.count() + 1)
}

/// The index of the turn [`turn_number`] numbers `number`, if there is one.
#[must_use]
pub fn turn_numbered(turns: &[Turn], number: usize) -> Option<usize> {
    (turns.iter().enumerate())
        .filter(|(_, turn)| turn.switch.is_none())
        .nth(number.checked_sub(1)?)
        .map(|(index, _)| index)
}

impl ChatNode {
    /// Applies `patch`. Changes to turns or items the chat doesn't have
    /// are ignored, so a confused daemon can't crash a client.
    pub fn apply(&mut self, patch: ChatPatch) {
        match patch {
            ChatPatch::Turn { index, turn } => {
                let index = index as usize;
                match index.cmp(&self.turns.len()) {
                    std::cmp::Ordering::Less => self.turns[index] = turn,
                    std::cmp::Ordering::Equal => self.turns.push(turn),
                    std::cmp::Ordering::Greater => {}
                }
            }
            ChatPatch::Item { turn, item, value } => {
                if let Some(turn) = self.turns.get_mut(turn as usize) {
                    let item = item as usize;
                    match item.cmp(&turn.items.len()) {
                        std::cmp::Ordering::Less => turn.items[item] = value,
                        std::cmp::Ordering::Equal => turn.items.push(value),
                        std::cmp::Ordering::Greater => {}
                    }
                }
            }
            ChatPatch::Append { turn, item, text } => {
                if let Some(Item::Words(words) | Item::Thought(words)) = self
                    .turns
                    .get_mut(turn as usize)
                    .and_then(|turn| turn.items.get_mut(item as usize))
                {
                    words.push_str(&text);
                }
            }
            ChatPatch::Output { turn, item, text } => {
                if let Some(Item::Tool(tool)) = self
                    .turns
                    .get_mut(turn as usize)
                    .and_then(|turn| turn.items.get_mut(item as usize))
                {
                    tool.append_output(&text);
                }
            }
            ChatPatch::State(state) => self.state = state,
            ChatPatch::Question(question) => self.question = question,
            ChatPatch::Notice(notice) => self.notice = notice,
            ChatPatch::Settings(settings) => self.settings = settings,
            ChatPatch::Usage(usage) => self.usage = usage,
            ChatPatch::Files { turn, files } => {
                if let Some(turn) = self.turns.get_mut(turn as usize) {
                    turn.files = files;
                }
            }
            ChatPatch::Agent(agent) => self.agent = agent,
            ChatPatch::Providers(providers) => self.providers = providers,
            ChatPatch::Subagents(subagents) => self.subagents = subagents,
        }
    }

    /// The chat as plain text: each message and what the agent said.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = String::new();
        for turn in &self.turns {
            if let Some(switch) = &turn.switch {
                let _ = write!(text, "— {} —\n\n", switch.label());
            }
            if !turn.prompt.is_empty() {
                text.push_str("> ");
                text.push_str(&turn.prompt.replace('\n', "\n> "));
                text.push_str("\n\n");
            }
            for item in &turn.items {
                match item {
                    Item::Words(words) => {
                        text.push_str(words.trim_end());
                        text.push_str("\n\n");
                    }
                    Item::Thought(_) => {}
                    Item::Tool(tool) => {
                        let mark = match tool.status {
                            ToolStatus::Running => "…",
                            ToolStatus::Done => "✓",
                            ToolStatus::Failed => "✗",
                        };
                        let _ = write!(text, "{mark} {}\n\n", tool.title);
                    }
                }
            }
            if let Some(TurnEnd {
                outcome: Outcome::Failed(why),
                ..
            }) = &turn.end
            {
                let _ = write!(text, "({why})\n\n");
            }
        }
        text.truncate(text.trim_end().len());
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(prompt: &str, items: Vec<Item>) -> Turn {
        Turn {
            prompt: prompt.into(),
            sent: 0,
            items,
            end: None,
            files: None,
            from: None,
            switch: None,
        }
    }

    #[test]
    fn turns_are_numbered_without_those_where_the_chat_moved() {
        let mut moved = turn("", vec![]);
        moved.switch = Some(Switch {
            from: "Claude Code".into(),
            to: "Codex".into(),
            model: None,
            resumed: false,
            notes: Vec::new(),
        });
        let turns = [turn("a", vec![]), moved, turn("b", vec![])];
        let numbers: Vec<Option<usize>> = (0..4).map(|at| turn_number(&turns, at)).collect();
        assert_eq!(numbers, [Some(1), None, Some(2), None]);
        let indices: Vec<Option<usize>> = (0..4).map(|n| turn_numbered(&turns, n)).collect();
        assert_eq!(indices, [None, Some(0), Some(2), None]);
    }

    #[test]
    fn patches_add_turns_and_stream_words() {
        let mut chat = ChatNode::default();
        chat.apply(ChatPatch::Turn {
            index: 0,
            turn: turn("hi", vec![Item::Words("Hel".into())]),
        });
        chat.apply(ChatPatch::Append {
            turn: 0,
            item: 0,
            text: "lo".into(),
        });
        // Out of range: ignored.
        chat.apply(ChatPatch::Append {
            turn: 3,
            item: 0,
            text: "x".into(),
        });
        chat.apply(ChatPatch::Turn {
            index: 5,
            turn: turn("lost", vec![]),
        });
        assert_eq!(chat.turns, [turn("hi", vec![Item::Words("Hello".into())])]);
        assert_eq!(chat.text(), "> hi\n\nHello");
    }
}
