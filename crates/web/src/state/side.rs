//! The phone's parent-scoped side sheet. Drafts never enter the parent transcript.
use super::{Asked, Online, State};
use crate::{
    action::Action,
    ui::{Element, el},
};
use clackatoo_protocol::{
    ChatNode, ContextRef, ContextReply, ContextSelection, Item, Request, Response, SideAction,
    TerminalId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Open,
    Hide,
    Send,
    Stop,
    Discard,
    Keep,
    Attach(u32),
    Copy(u32),
}
impl Control {
    pub(crate) fn encode(self) -> String {
        match self {
            Self::Open => "open".into(),
            Self::Hide => "hide".into(),
            Self::Send => "send".into(),
            Self::Stop => "stop".into(),
            Self::Discard => "discard".into(),
            Self::Keep => "keep".into(),
            Self::Attach(t) => format!("attach:{t}"),
            Self::Copy(t) => format!("copy:{t}"),
        }
    }
    pub(crate) fn decode(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "hide" => Self::Hide,
            "send" => Self::Send,
            "stop" => Self::Stop,
            "discard" => Self::Discard,
            "keep" => Self::Keep,
            _ => {
                let (k, n) = s.split_once(':')?;
                let n = n.parse().ok()?;
                match k {
                    "attach" => Self::Attach(n),
                    "copy" => Self::Copy(n),
                    _ => return None,
                }
            }
        })
    }
}
#[derive(Debug, Default)]
pub(crate) struct Side {
    pub open: bool,
    pub parent: Option<TerminalId>,
    pub node: Option<Box<ChatNode>>,
    pub drafts: std::collections::BTreeMap<TerminalId, String>,
    pub status: String,
    pending: Option<SideAction>,
    queued: Option<SideAction>,
    command: Option<String>,
    pub serial: u64,
    due: f64,
}
impl State {
    pub(super) fn side_lost(&mut self) {
        if self.side.pending.take().is_some() {
            self.side.status =
                "Connection lost before confirmation. Check the side conversation before retrying."
                    .into();
        }
        self.side.command = None;
        self.side.queued = None;
        self.side.serial = self.side.serial.wrapping_add(1);
        self.side.due = 0.0;
    }

    fn side_request(&mut self, action: SideAction) {
        let Some(parent) = self.open.as_ref().map(|o| o.id) else {
            return;
        };
        if self.online != Online::Open || !self.run_confirmed {
            self.side.status = "You're offline. The side draft is kept.".into();
            return;
        }
        if self.side.pending.is_some() {
            if !matches!(action, SideAction::Read) {
                self.side.queued = Some(action);
            }
            return;
        }
        if matches!(action, SideAction::Open { .. } | SideAction::Send { .. }) {
            self.side.open = true;
        }
        self.side.serial = self.side.serial.wrapping_add(1);
        self.side.pending = Some(action.clone());
        self.request(
            Request::Side {
                chat: parent,
                action,
            },
            Asked::Side {
                chat: parent,
                serial: self.side.serial,
            },
        );
    }
    pub(crate) fn side_open(&mut self, question: String, command: Option<String>) {
        let Some(parent) = self.open.as_ref().map(|o| o.id) else {
            return;
        };
        if self.side.parent != Some(parent) {
            self.side.node = None;
            self.side.pending = None;
            self.side.queued = None;
            self.side.serial = self.side.serial.wrapping_add(1);
        }
        self.side.parent = Some(parent);
        self.side.open = true;
        self.side.command = command;
        self.side.status = "Opening side conversation…".into();
        self.side_request(SideAction::Open { question });
        self.dirty.footer = true;
    }
    pub(crate) fn side_control(&mut self, control: Control) {
        let Some(parent) = self.open.as_ref().map(|o| o.id) else {
            return;
        };
        match control {
            Control::Open => self.side_open(String::new(), None),
            Control::Hide => self.side.open = false,
            Control::Send => {
                let text = self.side.drafts.get(&parent).cloned().unwrap_or_default();
                if !text.trim().is_empty() {
                    self.side_request(SideAction::Send { text });
                }
            }
            Control::Stop => self.side_request(SideAction::Stop),
            Control::Discard => self.side_request(SideAction::Discard),
            Control::Keep => self.side_request(SideAction::Keep),
            Control::Attach(turn) => self.side_request(SideAction::Attach { turn }),
            Control::Copy(_) => {} // handled by the browser clipboard API
        }
        self.dirty.footer = true;
    }
    pub(crate) fn side_poll(&mut self) {
        if self.side.open
            && self.open.as_ref().map(|o| o.id) == self.side.parent
            && self.now >= self.side.due
        {
            self.side_request(SideAction::Read);
        }
    }
    pub(crate) fn side_answer(&mut self, parent: TerminalId, serial: u64, response: Response) {
        if self.side.parent != Some(parent) || self.side.serial != serial {
            return;
        }
        let action = self.side.pending.take();
        self.side.due = self.now + 500.0;
        match response {
            Response::Side(reply) => {
                let lost = reply.node.is_none()
                    && matches!(action, Some(SideAction::Read))
                    && self.side.drafts.get(&parent).is_some_and(|d| !d.is_empty());
                if reply.node.is_none() && !lost {
                    self.side.open = false;
                    if matches!(action, Some(SideAction::Discard | SideAction::Keep)) {
                        self.side.drafts.remove(&parent);
                    }
                    self.note = Some(reply.notice.clone());
                }
                if self.side.node != reply.node || self.side.status != reply.notice {
                    self.dirty.footer = true;
                }
                self.side.node = reply.node;
                self.side.status = if lost {
                    "The temporary session ended. Your draft is kept; sending starts a fresh side conversation.".into()
                } else {
                    reply.notice
                };
                if lost {
                    self.side.due = f64::INFINITY;
                }
                if let Some(SideAction::Send { text }) = &action
                    && self.side.drafts.get(&parent) == Some(text)
                {
                    self.side.drafts.remove(&parent);
                }
                if matches!(action, Some(SideAction::Open { .. }))
                    && let Some(command) = self.side.command.take()
                    && self.drafts.get(&parent) == Some(&command)
                {
                    self.drafts.remove(&parent);
                    self.drafts_changed.insert(parent);
                    self.tell_draft();
                }
            }
            Response::Context(ContextReply::Snapshot(snapshot)) => {
                let count = self.context.attached.get(&parent).map_or(0, Vec::len)
                    + self.images.get(&parent).map_or(0, Vec::len);
                if count < clackatoo_protocol::ContextSnapshot::MAX_COUNT {
                    self.context
                        .attached
                        .entry(parent)
                        .or_default()
                        .push(ContextSelection {
                            source: ContextRef::Retained {
                                id: snapshot.id,
                                grant: snapshot.grant,
                            },
                            mode: snapshot.mode,
                            expected: Some(snapshot.revision),
                        });
                    self.side.open = false;
                    self.context.touched.insert(parent);
                    self.tell_draft();
                    self.note =
                        Some("Side answer attached to the main draft. Send when ready.".into());
                } else {
                    self.side.status = "The parent draft already has 32 attachments.".into();
                }
            }
            Response::Error(error) => {
                self.side.status = error.to_string();
                self.side.command = None;
                self.side.due = f64::INFINITY;
            }
            _ => {}
        }
        if let Some(action) = self.side.queued.take()
            && self.open.as_ref().is_some_and(|o| o.id == parent)
        {
            self.side_request(action);
        }
        self.dirty.footer = true;
    }
    pub(crate) fn side_copy(&self, turn: u32) -> String {
        self.side
            .node
            .as_ref()
            .and_then(|n| n.turns.get(turn as usize))
            .map(|t| {
                t.items
                    .iter()
                    .filter_map(|i| {
                        if let Item::Words(s) = i {
                            Some(s.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
    pub(crate) fn context_panels(&self) -> Element {
        self.context_panel().child(self.side_panel())
    }
    pub(crate) fn side_panel(&self) -> Element {
        let button = |text: &str, control| {
            el("button")
                .class_if(control != Control::Send, "quiet")
                .action(&Action::Side(control))
                .text(text)
        };
        let Some(parent) = self.open.as_ref().map(|o| o.id) else {
            return el("div");
        };
        let mut panel = el("div").child(button("Side question", Control::Open));
        if !self.side.open || self.side.parent != Some(parent) {
            return panel;
        }
        let working = self
            .open
            .as_ref()
            .and_then(|o| o.view.as_ref()?.chat())
            .is_some_and(super::busy);
        let title = self
            .open
            .as_ref()
            .and_then(|o| o.view.as_ref())
            .map_or("Chat", |v| v.title.as_str());
        let mut body = el("div")
            .class("side-conversation")
            .child(el("p").class("muted").text(format!(
                "{title} · Main agent {} · No tools",
                if working { "working" } else { "idle" }
            )))
            .child(el("p").class("muted").text(&self.side.status));
        let mut turns = el("div").class("side-turns");
        if let Some(node) = &self.side.node {
            for (index, turn) in node.turns.iter().enumerate() {
                let mut entry = el("article").child(el("div").class("prompt").text(&turn.prompt));
                for item in &turn.items {
                    if let Item::Words(text) = item {
                        entry = entry.child(
                            el("div")
                                .class("words")
                                .children(crate::markdown::render(text)),
                        );
                    }
                }
                if turn.end.is_some() {
                    entry = entry
                        .child(button(
                            "Send to main",
                            Control::Attach(u32::try_from(index).expect("bounded side history")),
                        ))
                        .child(button(
                            "Copy",
                            Control::Copy(u32::try_from(index).expect("bounded side history")),
                        ));
                }
                turns = turns.child(entry);
            }
            if super::busy(node) {
                turns = turns.child(el("p").class("muted").text("Answering…"));
            }
            if let Some(notice) = &node.notice {
                turns = turns.child(el("p").class("muted").text(notice));
            }
        }
        body = body.child(turns).child(
            el("textarea")
                .attr("id", "side-draft")
                .attr("aria-label", "Side question")
                .attr("placeholder", "Ask about this conversation…")
                .text(self.side.drafts.get(&parent).map_or("", String::as_str)),
        );
        let busy = self.side.node.as_ref().is_some_and(|n| super::busy(n));
        body = body
            .child(button(
                if busy { "Stop side answer" } else { "Send" },
                if busy { Control::Stop } else { Control::Send },
            ))
            .child(button("Keep as chat", Control::Keep))
            .child(button("Discard", Control::Discard));
        panel = panel.child(crate::view::sheet(
            "Side question",
            &Action::Side(Control::Hide),
            body,
        ));
        panel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{OpenChat, State};
    use clackatoo_protocol::{ClientMessage, SideReply};
    fn state() -> State {
        let mut state = State::default();
        state.connected_now();
        state.take_outbox();
        state.asked.clear();
        state.open = Some(OpenChat {
            id: TerminalId(3),
            capabilities: vec![],
            picker: crate::settings::Picker::default(),
            view: None,
            folds: std::collections::BTreeMap::default(),
            unacknowledged: false,
            history_pending: None,
        });
        state
    }
    #[test]
    fn btw_is_intercepted_and_only_clears_its_exact_draft_after_acceptance() {
        let mut state = state();
        state.drafts.insert(TerminalId(3), "/btw Why?".into());
        state.submit(false);
        assert!(
            matches!(&state.take_outbox()[0],ClientMessage::Request(Request::Side{chat:TerminalId(3),action:SideAction::Open{question}})if question=="Why?")
        );
        assert_eq!(state.drafts[&TerminalId(3)], "/btw Why?");
        state
            .drafts
            .insert(TerminalId(3), "A newer main draft".into());
        state.receive(clackatoo_protocol::ServerMessage::Response(Response::Side(
            SideReply {
                node: Some(Box::default()),
                notice: "Captured".into(),
            },
        )));
        assert_eq!(state.drafts[&TerminalId(3)], "A newer main draft");
        assert!(state.side.open);
    }
    #[test]
    fn reconnect_never_replays_a_side_question_and_controls_round_trip() {
        let mut state = state();
        state.side_open("Question".into(), None);
        state.take_outbox();
        state.lost(1);
        state.connected_now();
        state.take_outbox();
        state.asked.clear();
        state.now = 1000.0;
        state.side_poll();
        assert!(matches!(
            &state.take_outbox()[0],
            ClientMessage::Request(Request::Side {
                action: SideAction::Read,
                ..
            })
        ));
        for c in [
            Control::Open,
            Control::Hide,
            Control::Send,
            Control::Stop,
            Control::Keep,
            Control::Discard,
            Control::Attach(19),
            Control::Copy(4),
        ] {
            assert_eq!(Control::decode(&c.encode()), Some(c));
        }
    }
    #[test]
    fn controls_wait_for_a_poll_and_a_lost_session_keeps_the_unsent_followup() {
        let mut state = state();
        state.side_open(String::new(), None);
        state.take_outbox();
        state.receive(clackatoo_protocol::ServerMessage::Response(Response::Side(
            SideReply {
                node: Some(Box::default()),
                notice: "Captured".into(),
            },
        )));
        state.now = 1000.0;
        state.side_poll();
        state.take_outbox();
        state.side_control(Control::Stop);
        state.receive(clackatoo_protocol::ServerMessage::Response(Response::Side(
            SideReply {
                node: Some(Box::default()),
                notice: "Captured".into(),
            },
        )));
        assert!(matches!(
            &state.take_outbox()[0],
            ClientMessage::Request(Request::Side {
                action: SideAction::Stop,
                ..
            })
        ));
        state.receive(clackatoo_protocol::ServerMessage::Response(Response::Side(
            SideReply {
                node: Some(Box::default()),
                notice: "Stopped".into(),
            },
        )));
        state
            .side
            .drafts
            .insert(TerminalId(3), "Unsent follow-up".into());
        state.now = 2000.0;
        state.side_poll();
        state.take_outbox();
        state.receive(clackatoo_protocol::ServerMessage::Response(Response::Side(
            SideReply {
                node: None,
                notice: String::new(),
            },
        )));
        assert_eq!(state.side.drafts[&TerminalId(3)], "Unsent follow-up");
        assert!(state.side.open);
    }
}
