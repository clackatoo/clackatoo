//! What tapping something does, written into its element's
//! `data-action`, so one listener on the page handles every tap, and no
//! element needs a closure of its own that would have to be freed.

use clackatoo_protocol::TerminalId;

/// What tapping something does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Shows the list of chats.
    Home,
    /// Shows a chat.
    Open(TerminalId),
    /// Answers question `question` with its choice at `choice`.
    Answer { question: u64, choice: usize },
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
            _ => return None,
        })
    }
}

/// What picking from a chat's menus does: a menu's `<select>` names its
/// setting in `data-setting`, and each option says what it does in its
/// value.
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
        ] {
            assert_eq!(Action::decode(&action.encode()), Some(action));
        }
        assert_eq!(Action::decode("open:x"), None);
        assert_eq!(Action::decode("answer:1"), None);
        assert_eq!(Action::decode("explode"), None);
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
