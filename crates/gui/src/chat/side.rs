//! A side sheet reuses a full chat view, with requests scoped to its parent.
use super::{ChatView, Settled};
use crate::{fonts::Fonts, input::KeyPress};
use clackatoo_protocol::{
    ChatAction, ContextRef, ContextReply, ContextSelection, Response, SideAction,
};
use std::time::{Duration, Instant};
use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug)]
pub(super) enum Control {
    Hide,
    Keep,
    Discard,
}

#[derive(Debug, Default)]
pub(super) struct SideUi {
    pub open: bool,
    pub focused: bool,
    pub child: Option<Box<ChatView>>,
    pub status: String,
    pub body: [f32; 4],
    pub controls: Vec<(Control, [f32; 4])>,
    pub due: Option<Instant>,
    requests: std::collections::VecDeque<SideAction>,
    pending: Option<(u64, SideAction)>,
    next: u64,
}
impl SideUi {
    pub(super) fn request(&mut self, action: SideAction) {
        if matches!(action, SideAction::Read)
            && (self.pending.is_some() || !self.requests.is_empty())
        {
            return;
        }
        if self.requests.len() < 8 {
            self.requests.push_back(action);
        }
    }
}
impl ChatView {
    pub(crate) fn open_side(&mut self, question: String) {
        if !self.side_supported {
            self.say("Side questions need a newer daemon.".into());
            return;
        }
        self.menu = None;
        self.context.open = false;
        self.side.open = true;
        self.side.status = "Opening side conversation…".into();
        self.side.request(SideAction::Open { question });
    }
    pub(super) fn side_due(&self) -> Option<Instant> {
        if !self.offline
            && self.side.open
            && self.side.pending.is_none()
            && self.side.requests.is_empty()
        {
            self.side.due
        } else {
            None
        }
    }
    pub(super) fn side_unsettled(&self) -> bool {
        (!self.side.requests.is_empty() && self.side.pending.is_none())
            || self.side_due().is_some_and(|d| Instant::now() >= d)
            || (self.side.open && self.side.child.as_ref().is_some_and(|c| c.unsettled()))
    }
    pub(crate) fn side_reconnected(&mut self) {
        // An unconfirmed question remains editable, but is never replayed.
        let questions = self
            .side
            .pending
            .take()
            .map(|(_, a)| a)
            .into_iter()
            .chain(self.side.requests.drain(..))
            .filter_map(|a| match a {
                SideAction::Open { question } if !question.is_empty() => Some(question),
                SideAction::Send { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !questions.is_empty() {
            let child = self.side.child.get_or_insert_with(|| {
                let mut view = ChatView::new(
                    "Side question".into(),
                    clackatoo_protocol::ChatNode {
                        state: clackatoo_protocol::ChatState::Ready,
                        ..Default::default()
                    },
                );
                view.side_child = true;
                Box::new(view)
            });
            let current = child.editor.raw_text();
            let mut text = questions.join("\n\n");
            if !current.is_empty() {
                text.push_str("\n\n");
                text.push_str(current);
            }
            child.editor.set_text(&text);
            self.side.status =
                "Delivery was unconfirmed. Check the last exchange before retrying.".into();
        }
        if self.side.open {
            self.side.request(SideAction::Read);
        }
    }
    pub(super) fn settle_side(&mut self, fonts: &mut Fonts) -> Settled {
        let mut settled = Settled::default();
        if self.side.open
            && let Some(child) = &mut self.side.child
        {
            let mut result = child.settle(fonts);
            for action in result.actions {
                let action = match action {
                    ChatAction::Send(text)
                    | ChatAction::SendImages { text, .. }
                    | ChatAction::Submit { text, .. }
                    | ChatAction::SubmitContext { text, .. } => Some(SideAction::Send { text }),
                    ChatAction::Stop => Some(SideAction::Stop),
                    _ => None,
                };
                if let Some(action) = action {
                    self.side.requests.push_back(action);
                }
            }
            self.side.requests.extend(child.side.requests.drain(..));
            settled.copied.append(&mut result.copied);
            settled.redraw |= result.redraw;
        }
        if self.side_due().is_some_and(|d| Instant::now() >= d) {
            self.side.request(SideAction::Read);
        }
        settled
    }
    pub(crate) fn take_side_requests(&mut self) -> Vec<(u64, SideAction)> {
        if self.side.pending.is_some() {
            return Vec::new();
        }
        let Some(action) = self.side.requests.pop_front() else {
            return Vec::new();
        };
        self.side.next = self.side.next.wrapping_add(1);
        self.side.pending = Some((self.side.next, action.clone()));
        vec![(self.side.next, action)]
    }
    pub(crate) fn side_answer(&mut self, serial: u64, response: Response) {
        if self.side.pending.as_ref().is_none_or(|(n, _)| *n != serial) {
            return;
        }
        let (_, action) = self.side.pending.take().expect("checked");
        self.side.due = Some(Instant::now() + Duration::from_millis(350));
        match response {
            Response::Side(reply) => {
                if let Some(node) = reply.node {
                    if let Some(child) = &mut self.side.child {
                        let changed = child.node != *node;
                        child.node = *node;
                        if changed {
                            child.came_whole();
                            for index in 0..child.cache.turns.len() {
                                child.restyled(index);
                            }
                        }
                    } else {
                        let mut child = ChatView::new("Side question".into(), *node);
                        child.side_child = true;
                        child.resend_supported = false;
                        self.side.child = Some(Box::new(child));
                    }
                    self.side.status = reply.notice;
                } else if matches!(action, SideAction::Read)
                    && self
                        .side
                        .child
                        .as_ref()
                        .is_some_and(|c| !c.editor.raw_text().is_empty())
                {
                    if let Some(child) = &mut self.side.child {
                        child.node = clackatoo_protocol::ChatNode {
                            state: clackatoo_protocol::ChatState::Ready,
                            ..Default::default()
                        };
                        child.came_whole();
                    }
                    self.side.status="The temporary session ended. Your draft is kept; sending starts a fresh side conversation.".into();
                    self.side.due = None;
                } else {
                    self.side.child = None;
                    self.side.open = false;
                    self.side.requests.clear();
                    if !reply.notice.is_empty() {
                        self.say(reply.notice);
                    }
                }
            }
            Response::Context(ContextReply::Snapshot(snapshot)) => {
                if self.context.attached.len() + self.images.len()
                    < clackatoo_protocol::ContextSnapshot::MAX_COUNT
                {
                    self.context.attached.push(super::context::Attached {
                        selection: ContextSelection {
                            source: ContextRef::Retained {
                                id: snapshot.id.clone(),
                                grant: snapshot.grant.clone(),
                            },
                            mode: snapshot.mode,
                            expected: Some(snapshot.revision.clone()),
                        },
                        ready: true,
                        preview: snapshot,
                    });
                    self.context.dirty = true;
                    self.side.open = false;
                    self.say(
                        "Side answer attached to this draft. Add a message and send when ready."
                            .into(),
                    );
                } else {
                    self.side.status = "The parent draft already has 32 attachments.".into();
                }
            }
            Response::Error(error) => {
                self.side.status = error.to_string();
                let restore = match action {
                    SideAction::Open { question } => Some(question),
                    SideAction::Send { text } => Some(text),
                    _ => None,
                };
                if let Some(text) = restore.filter(|t| !t.is_empty()) {
                    if self.side.child.is_none() {
                        let mut child = ChatView::new(
                            "Side question".into(),
                            clackatoo_protocol::ChatNode::default(),
                        );
                        child.side_child = true;
                        self.side.child = Some(Box::new(child));
                    }
                    if let Some(child) = &mut self.side.child {
                        child.put_recalled(text);
                    }
                }
                // An unavailable side stays visible with the error, without polling forever.
                self.side.due = None;
            }
            _ => {}
        }
    }
    pub(super) fn side_local(&self, at: [f32; 2]) -> [f32; 2] {
        [at[0] - self.side.body[0], at[1] - self.side.body[1]]
    }
    pub(super) fn side_key(&mut self, press: &KeyPress<'_>) -> bool {
        // Let the app read plain text from the system clipboard, then route
        // the paste through the parent's stable pane to this composer.
        if press.modifiers.ctrl
            && matches!(press.key,Key::Character(s) if s.eq_ignore_ascii_case("v"))
        {
            return false;
        }
        if *press.key == Key::Named(NamedKey::Escape) {
            self.side.open = false;
            return true;
        }
        if let Some(child) = &mut self.side.child {
            child.key(press);
        }
        true
    }
    pub(super) fn side_press(&mut self, at: [f32; 2], clicks: u8, shift: bool) {
        let control = self
            .side
            .controls
            .iter()
            .find(|(_, r)| super::contains(*r, at))
            .map(|(c, _)| *c);
        match control {
            Some(Control::Hide) => self.side.open = false,
            Some(Control::Keep) => self.side.request(SideAction::Keep),
            Some(Control::Discard) => self.side.request(SideAction::Discard),
            None if super::contains(self.side.body, at) => {
                let at = self.side_local(at);
                if let Some(child) = &mut self.side.child {
                    child.press(at, clicks, shift);
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::fixtures::{fonts, talk};
    use clackatoo_protocol::SideReply;
    #[test]
    fn btw_is_host_owned_and_never_sends_to_the_main_chat() {
        let mut view = ChatView::new("Parent".into(), talk(&[("Main task", "Working")]));
        view.side_supported = true;
        view.editor.set_text("/btw Why cookies?");
        let mut settled = Settled::default();
        assert!(view.native_command(&mut settled));
        assert!(settled.actions.is_empty());
        assert!(view.editor.raw_text().is_empty());
        assert_eq!(view.node.turns.len(), 1);
        let requests = view.take_side_requests();
        assert_eq!(
            requests[0].1,
            SideAction::Open {
                question: "Why cookies?".into()
            }
        );
        view.side_answer(
            requests[0].0,
            Response::Side(SideReply {
                node: Some(Box::new(talk(&[("Why cookies?", "For browser sessions.")]))),
                notice: "Captured".into(),
            }),
        );
        assert!(view.side.open);
        view.side.open = false;
        view.open_side(String::new());
        assert_eq!(view.side.child.as_ref().unwrap().node.turns.len(), 1);
    }
    #[test]
    fn sheet_keeps_main_draft_and_uses_the_regular_composer_and_layout() {
        let mut fonts = fonts();
        let mut view = ChatView::new("Parent".into(), talk(&[("Task", "Answer")]));
        view.side_supported = true;
        view.editor.set_text("Unsent main draft");
        view.open_side(String::new());
        let serial = view.take_side_requests()[0].0;
        view.side_answer(
            serial,
            Response::Side(SideReply {
                node: Some(Box::new(talk(&[(
                    "Question",
                    "## Answer\n\nUse **cookies**.",
                )]))),
                notice: "Captured".into(),
            }),
        );
        view.paste("Follow-up");
        view.settle(&mut fonts);
        assert_eq!(view.editor.raw_text(), "Unsent main draft");
        assert_eq!(
            view.side.child.as_ref().unwrap().editor.raw_text(),
            "Follow-up"
        );
        for width in [360.0, 1280.0] {
            view.paint(&mut fonts, [0.0; 2], [width, 780.0], true);
            assert!(view.side.body[0] >= 0.0);
            assert!(view.side.body[0] + view.side.body[2] <= width);
            assert!(view.side.body[3] > 0.0);
        }
    }
    #[test]
    fn reconnect_keeps_unconfirmed_questions_without_replaying_them() {
        let mut view = ChatView::new("Parent".into(), talk(&[]));
        view.side_supported = true;
        view.open_side("Unconfirmed question".into());
        view.take_side_requests();
        view.side_reconnected();
        assert_eq!(
            view.side.child.as_ref().unwrap().editor.raw_text(),
            "Unconfirmed question"
        );
        let request = view.take_side_requests();
        assert_eq!(request[0].1, SideAction::Read);
        view.side_answer(
            request[0].0,
            Response::Side(SideReply {
                node: None,
                notice: String::new(),
            }),
        );
        assert!(view.side.open);
        assert_eq!(
            view.side.child.as_ref().unwrap().editor.raw_text(),
            "Unconfirmed question"
        );
    }
}
