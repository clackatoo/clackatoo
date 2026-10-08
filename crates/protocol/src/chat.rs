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
    /// The turns, oldest first.
    pub turns: Vec<Turn>,
    /// What the chat is doing.
    pub state: ChatState,
    /// What the agent is asking, waiting for an answer.
    pub question: Option<Question>,
    /// Something to tell the user that belongs to no turn, such as that
    /// the agent stopped while the chat was idle.
    pub notice: Option<String>,
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
    /// The chat's state is now this.
    State(ChatState),
    /// The agent's question is now this.
    Question(Option<Question>),
    /// The notice is now this.
    Notice(Option<String>),
}

/// What a client asks of a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatAction {
    /// Send this message, starting a turn.
    Send(String),
    /// Stop the agent answering.
    Stop,
    /// Answer the agent's question with the choice with this ID, or
    /// with none.
    Answer(Option<String>),
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
            ChatPatch::Append { turn, item, text } => {
                if let Some(Item::Words(words) | Item::Thought(words)) = self
                    .turns
                    .get_mut(turn as usize)
                    .and_then(|turn| turn.items.get_mut(item as usize))
                {
                    words.push_str(&text);
                }
            }
            ChatPatch::State(state) => self.state = state,
            ChatPatch::Question(question) => self.question = question,
            ChatPatch::Notice(notice) => self.notice = notice,
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
