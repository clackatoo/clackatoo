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
        }
    }

    /// The chat as plain text: each message and what the agent said.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = String::new();
        for turn in &self.turns {
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
        }
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
