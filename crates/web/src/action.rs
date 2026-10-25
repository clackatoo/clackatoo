//! What tapping something does, written into its element's
//! `data-action`, so one listener on the page handles every tap, and no
//! element needs a closure of its own that would have to be freed.

use clackatoo_protocol::TerminalId;

use crate::keys::{Key, Modifiers};
use crate::state::HomeFilter;

/// What tapping something does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Shows the list of chats.
    Home,
    /// Shows a chat.
    Open(TerminalId),
    /// Answers question `question` with its choice at `choice`.
    Answer {
        question: u64,
        choice: usize,
    },
    /// Stops the agent answering.
    Stop,
    /// Sends what's typed.
    Send,
    /// Asks for notifications.
    Notify,
    /// Pairs with the password typed.
    Pair,
    /// Asks a window to allow the device.
    Ask,
    /// Gives the password again, as it's asked for.
    Renew,
    /// Starts pairing again, after an ask went unanswered.
    Again,
    /// Pairs here, in the browser, though an app on the home screen
    /// wouldn't share what pairing here stores.
    PairHere,
    /// Shows a terminal.
    OpenTerminal(TerminalId),
    /// Fits the terminal shown to the phone, for now.
    Fit,
    /// Puts the terminal shown back to its own size.
    Unfit,
    /// Brings up the keyboard, to type into the terminal shown.
    Type,
    /// A key on the row above the keyboard.
    Key(RowKey),
    /// Shows how much context the chat shown fills, in full, or hides it.
    Usage,
    Settings,
    Permissions,
    Capabilities,
    CancelActivity(usize),
    Latest,
    OlderChat,
    Filter(HomeFilter),
    Steer,
    RemoveQueued(u64),
    SendQueued(u64),
}

/// The keys on the row above the phone's keyboard: those a phone's
/// keyboard hasn't, or hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKey {
    Escape,
    Tab,
    /// Ctrl, for the next key.
    Ctrl,
    Left,
    Up,
    Down,
    Right,
    Pipe,
    Tilde,
    Slash,
    Dash,
    /// Ctrl+C, at one tap.
    CtrlC,
    /// Pastes what's on the clipboard.
    Paste,
}

impl RowKey {
    /// Every key on the row, in its order.
    pub const ALL: [Self; 13] = [
        Self::Escape,
        Self::Tab,
        Self::Ctrl,
        Self::CtrlC,
        Self::Left,
        Self::Up,
        Self::Down,
        Self::Right,
        Self::Pipe,
        Self::Tilde,
        Self::Slash,
        Self::Dash,
        Self::Paste,
    ];

    /// Its name, in `data-action`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Escape => "esc",
            Self::Tab => "tab",
            Self::Ctrl => "ctrl",
            Self::Left => "left",
            Self::Up => "up",
            Self::Down => "down",
            Self::Right => "right",
            Self::Pipe => "pipe",
            Self::Tilde => "tilde",
            Self::Slash => "slash",
            Self::Dash => "dash",
            Self::CtrlC => "ctrl-c",
            Self::Paste => "paste",
        }
    }

    /// What it says on it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Escape => "Esc",
            Self::Tab => "Tab",
            Self::Ctrl => "Ctrl",
            Self::Left => "←",
            Self::Up => "↑",
            Self::Down => "↓",
            Self::Right => "→",
            Self::Pipe => "|",
            Self::Tilde => "~",
            Self::Slash => "/",
            Self::Dash => "-",
            Self::CtrlC => "^C",
            Self::Paste => "Paste",
        }
    }

    /// The key it types, with what it holds, unless it types none of its
    /// own, as Ctrl and Paste don't.
    #[must_use]
    pub fn key(self) -> Option<(Key, Modifiers)> {
        let text = |text: &str| Key::Text(text.to_owned());
        let key = match self {
            Self::Escape => Key::Escape,
            Self::Tab => Key::Tab,
            Self::Left => Key::Left,
            Self::Up => Key::Up,
            Self::Down => Key::Down,
            Self::Right => Key::Right,
            Self::Pipe => text("|"),
            Self::Tilde => text("~"),
            Self::Slash => text("/"),
            Self::Dash => text("-"),
            Self::CtrlC => {
                let ctrl = Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                };
                return Some((text("c"), ctrl));
            }
            Self::Ctrl | Self::Paste => return None,
        };
        Some((key, Modifiers::default()))
    }
}

impl Action {
    /// As `data-action` says it.
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::Home => "home".to_owned(),
            Self::Open(chat) => format!("open:{chat}"),
            Self::Answer { question, choice } => format!("answer:{question}:{choice}"),
            Self::Stop => "stop".to_owned(),
            Self::Send => "send".to_owned(),
            Self::Notify => "notify".to_owned(),
            Self::Pair => "pair".to_owned(),
            Self::Ask => "ask".to_owned(),
            Self::Renew => "renew".to_owned(),
            Self::Again => "again".to_owned(),
            Self::PairHere => "pair-here".to_owned(),
            Self::OpenTerminal(terminal) => format!("term:{terminal}"),
            Self::Fit => "fit".to_owned(),
            Self::Unfit => "unfit".to_owned(),
            Self::Type => "type".to_owned(),
            Self::Key(key) => format!("key:{}", key.name()),
            Self::Usage => "usage".to_owned(),
            Self::Settings => "settings".to_owned(),
            Self::Permissions => "permissions".to_owned(),
            Self::Capabilities => "capabilities".to_owned(),
            Self::CancelActivity(index) => format!("cancel-activity:{index}"),
            Self::Latest => "latest".to_owned(),
            Self::OlderChat => "older-chat".to_owned(),
            Self::Filter(filter) => format!("filter:{}", filter.name()),
            Self::Steer => "steer".to_owned(),
            Self::RemoveQueued(id) => format!("remove-queued:{id}"),
            Self::SendQueued(id) => format!("send-queued:{id}"),
        }
    }

    /// What `data-action` says, if it says anything.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let (word, rest) = text.split_once(':').unwrap_or((text, ""));
        Some(match (word, rest) {
            ("home", "") => Self::Home,
            ("open", chat) => Self::Open(TerminalId(chat.parse().ok()?)),
            ("answer", rest) => {
                let (question, choice) = rest.split_once(':')?;
                Self::Answer {
                    question: question.parse().ok()?,
                    choice: choice.parse().ok()?,
                }
            }
            ("stop", "") => Self::Stop,
            ("send", "") => Self::Send,
            ("notify", "") => Self::Notify,
            ("pair", "") => Self::Pair,
            ("ask", "") => Self::Ask,
            ("renew", "") => Self::Renew,
            ("again", "") => Self::Again,
            ("pair-here", "") => Self::PairHere,
            ("term", terminal) => Self::OpenTerminal(TerminalId(terminal.parse().ok()?)),
            ("fit", "") => Self::Fit,
            ("unfit", "") => Self::Unfit,
            ("type", "") => Self::Type,
            ("key", name) => Self::Key(RowKey::ALL.into_iter().find(|key| key.name() == name)?),
            ("usage", "") => Self::Usage,
            ("settings", "") => Self::Settings,
            ("permissions", "") => Self::Permissions,
            ("capabilities", "") => Self::Capabilities,
            ("cancel-activity", value) => Self::CancelActivity(value.parse().ok()?),
            ("latest", "") => Self::Latest,
            ("older-chat", "") => Self::OlderChat,
            ("filter", name) => Self::Filter(
                HomeFilter::ALL
                    .into_iter()
                    .find(|filter| filter.name() == name)?,
            ),
            ("steer", "") => Self::Steer,
            ("remove-queued", id) => Self::RemoveQueued(id.parse().ok()?),
            ("send-queued", id) => Self::SendQueued(id.parse().ok()?),
            _ => return None,
        })
    }
}

/// What picking from a chat's settings does. Each choice names its setting
/// in `data-setting` and encodes its value in `data-pick`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Gives the setting with this ID this value.
    Set { setting: String, value: String },
    /// Moves the chat to another agent, on this model, or the agent's own
    /// choice.
    Switch {
        agent: String,
        model: Option<String>,
    },
}

impl Pick {
    /// An option's value, for a setting's menu.
    #[must_use]
    pub fn set_value(value: &str) -> String {
        format!("set:{value}")
    }

    /// An option's value, moving to `agent` on `model`.
    #[must_use]
    pub fn switch_value(agent: &str, model: Option<&str>) -> String {
        format!("switch:{agent}:{}", model.unwrap_or_default())
    }

    /// What picking the option `value` in the menu of `setting` does.
    #[must_use]
    pub fn decode(setting: &str, value: &str) -> Option<Self> {
        if let Some(value) = value.strip_prefix("set:") {
            return Some(Self::Set {
                setting: setting.to_owned(),
                value: value.to_owned(),
            });
        }
        let (agent, model) = value.strip_prefix("switch:")?.split_once(':')?;
        Some(Self::Switch {
            agent: agent.to_owned(),
            model: (!model.is_empty()).then(|| model.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_come_back_as_they_went() {
        for action in [
            Action::Home,
            Action::Open(TerminalId(12)),
            Action::Answer {
                question: 7,
                choice: 2,
            },
            Action::Stop,
            Action::Send,
            Action::Notify,
            Action::Pair,
            Action::Ask,
            Action::Renew,
            Action::Again,
            Action::PairHere,
            Action::OpenTerminal(TerminalId(3)),
            Action::Fit,
            Action::Unfit,
            Action::Type,
            Action::Usage,
        ]
        .into_iter()
        .chain(RowKey::ALL.map(Action::Key))
        {
            assert_eq!(Action::decode(&action.encode()), Some(action));
        }
        assert_eq!(Action::decode("open:x"), None);
        assert_eq!(Action::decode("answer:1"), None);
        assert_eq!(Action::decode("explode"), None);
        assert_eq!(Action::decode("key:f13"), None);
    }

    #[test]
    fn picks_say_what_they_do() {
        assert_eq!(
            Pick::decode("mode", &Pick::set_value("plan:fast")),
            Some(Pick::Set {
                setting: "mode".into(),
                value: "plan:fast".into()
            })
        );
        assert_eq!(
            Pick::decode("model", &Pick::switch_value("codex", Some("gpt:5"))),
            Some(Pick::Switch {
                agent: "codex".into(),
                model: Some("gpt:5".into())
            })
        );
        assert_eq!(
            Pick::decode("model", &Pick::switch_value("codex", None)),
            Some(Pick::Switch {
                agent: "codex".into(),
                model: None
            })
        );
        assert_eq!(Pick::decode("model", "nonsense"), None);
    }
}
