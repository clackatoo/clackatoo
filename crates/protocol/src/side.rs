//! Temporary side conversations, addressed through their parent.
use crate::ChatNode;
use serde::{Deserialize, Serialize};

/// A user action on a parent's temporary conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SideAction {
    /// Open or reopen; an optional question is sent only to the side session.
    Open {
        /// Initial question, or empty to reopen.
        question: String,
    },
    /// Refresh the side sheet, without creating a session.
    Read,
    /// Ask a follow-up.
    Send {
        /// Follow-up question.
        text: String,
    },
    /// Stop only the side answer.
    Stop,
    /// Permanently discard the temporary conversation.
    Discard,
    /// Promote it into a normal chat in a new tab.
    Keep,
    /// Preview one answer as context for the parent's draft.
    Attach {
        /// Zero-based turn in the side conversation.
        turn: u32,
    },
}

/// A snapshot of the side conversation. Its internal terminal ID is never exposed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideReply {
    /// None after discard or promotion, or if the parent has no side conversation.
    pub node: Option<Box<ChatNode>>,
    /// How the parent context was captured, and any limits.
    pub notice: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ClientMessage, Request, Response, ServerMessage, TerminalId,
        wire::{Downgrade, Downgraded, SIDE_QUESTIONS},
    };
    #[test]
    fn side_questions_refuse_old_daemons_and_reply_with_a_readable_fallback() {
        let request = ClientMessage::Request(Request::Side {
            chat: TerminalId(1),
            action: SideAction::Open {
                question: "why".into(),
            },
        });
        assert!(matches!(
            request.downgrade(SIDE_QUESTIONS - 1),
            Downgraded::Refused
        ));
        assert!(matches!(
            request.downgrade(SIDE_QUESTIONS),
            Downgraded::AsIs
        ));
        let response = ServerMessage::Response(Response::Side(SideReply {
            node: None,
            notice: "Discarded".into(),
        }));
        assert!(matches!(
            response.downgrade(SIDE_QUESTIONS - 1),
            Downgraded::To(ServerMessage::Response(Response::Error(_)))
        ));
    }
}
