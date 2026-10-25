//! A live window independently checks every daemon-routed automation request.
use std::cell::Cell;

use clackatoo_protocol::{
    ChatAction, ClientMessage, NativeAccess, NativeAction, NativeReply, Request, TerminalId,
};

use super::{App, MouseButton};

#[derive(Debug, Default)]
pub(super) struct Native {
    pub(super) pending: Option<(TerminalId, NativeAccess)>,
    pub(super) grant: Option<(TerminalId, NativeAccess)>,
    pub(super) revision: Cell<u64>,
    pub(super) snapshot: Option<u64>,
    controls: serde_json::Value,
    focus: String,
    pub(super) automating: bool,
    pub(super) refused: bool,
}

/// Input sent by an agent cannot approve a question or loosen permissions.
pub(super) fn changes_authority(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::NativeGrant { .. }
            | ClientMessage::Login { .. }
            | ClientMessage::Request(
                Request::Chat {
                    action: ChatAction::Answer { .. } | ChatAction::Set { .. },
                    ..
                } | Request::AnswerPairing { .. }
            )
    )
}

impl App {
    pub(super) fn revoke_native_click(&mut self, button: MouseButton) -> bool {
        if button == MouseButton::Left
            && !self.native.automating
            && let (Some((chat, _)), Some(point)) = (self.native.grant, self.pointer)
            && self.reconnecting().is_none()
            && self.out_of_contact().is_none()
            && self
                .chrome()
                .and_then(|c| c.alert)
                .is_some_and(|(_, rect)| rect.contains(point))
        {
            self.native.grant = None;
            self.native.snapshot = None;
            self.native.pending = Some((chat, clackatoo_protocol::NativeAccess::Off));
            if let Some(sender) = &mut self.sender {
                let _ = sender.send(&ClientMessage::NativeGrant {
                    chat,
                    access: clackatoo_protocol::NativeAccess::Off,
                });
            }
            self.update_title();
            self.request_redraw();
            return true;
        }
        false
    }

    pub(super) fn native_request(&mut self, id: u64, chat: TerminalId, action: NativeAction) {
        let result = self.native_act(chat, action);
        if let Some(sender) = &mut self.sender {
            let _ = sender.send(&ClientMessage::NativeReply { id, result });
        }
    }

    fn native_act(
        &mut self,
        chat: TerminalId,
        action: NativeAction,
    ) -> Result<NativeReply, String> {
        let access = self
            .native
            .grant
            .filter(|(owner, _)| *owner == chat)
            .map(|(_, access)| access)
            .ok_or("This window has not granted this chat access.")?;
        if access == NativeAccess::Off {
            return Err("Window access is off.".into());
        }
        if self.notice.is_some() {
            return Err(
                "A connection or authentication dialog is open. Native access is suspended.".into(),
            );
        }
        if action == NativeAction::Snapshot {
            let frame = self.frame.as_mut().ok_or("The window has no frame.")?;
            let [width, height] = frame.size();
            if u64::from(width) * u64::from(height) > clackatoo_protocol::ChatImage::MAX_PIXELS {
                return Err("Window is too large to capture safely.".into());
            }
            frame.capture = true;
            frame.captured = None;
            self.draw();
            let picture = self
                .frame
                .as_mut()
                .and_then(|f| f.captured.take())
                .ok_or("The window could not draw a snapshot.")?;
            let image = crate::clipboard_image::encode(&arboard::ImageData {
                width: picture.size[0] as usize,
                height: picture.size[1] as usize,
                bytes: picture.rgba.into(),
            })
            .map_err(|e| e.to_string())?;
            let revision = self.native.revision.get();
            self.native.snapshot = Some(revision);
            self.native.controls = self.desk.native_controls();
            self.native.focus = format!("{:?}", self.desk.shown());
            let description = serde_json::json!({
                "width":width,"height":height,"coordinates":"physical pixels from top left",
                "title":self.desk.title(),"focus":format!("{:?}",self.desk.shown()),
                "status":format!("{:?}",self.desk.status()),
                "menu_open":self.menu.is_some(),"key_help_open":self.shortcut_help.is_some(),
                "access":format!("{access:?}"),"controls":self.desk.native_controls(),
                "instruction":"Observe the image before acting. Every input consumes this snapshot; obtain another afterward."
            }).to_string();
            return Ok(NativeReply {
                revision,
                description,
                png: Some(image.data),
            });
        }
        if access != NativeAccess::Control {
            return Err("This window grants Inspect only. Control needs a human grant.".into());
        }
        let revision = match &action {
            NativeAction::Click { revision, .. }
            | NativeAction::Type { revision, .. }
            | NativeAction::Key { revision, .. } => *revision,
            NativeAction::Snapshot => unreachable!(),
        };
        if self.native.snapshot != Some(revision) || revision != self.native.revision.get() {
            return Err(
                "The snapshot is stale or already used. Take a new native_snapshot before acting."
                    .into(),
            );
        }
        self.draw();
        if self.native.focus != format!("{:?}", self.desk.shown()) {
            return Err("Focus changed since the snapshot; inspect again.".into());
        }
        if let NativeAction::Click { x, y, .. } = &action {
            let point = [f64::from(*x), f64::from(*y)];
            let current = self.desk.native_controls();
            if target_at(&self.native.controls, point) != target_at(&current, point) {
                return Err("The clicked control moved or changed; inspect again.".into());
            }
        }
        self.native.snapshot = None;
        self.native
            .revision
            .set(self.native.revision.get().wrapping_add(1));
        self.native.automating = true;
        self.native.refused = false;
        let result = self.native_input(action);
        self.send_outbox();
        self.native.automating = false;
        self.request_redraw();
        result?;
        if self.native.refused {
            return Err("Automated input cannot grant access, change permissions, answer approval questions, or open external windows.".into());
        }
        Ok(NativeReply {
            revision: self.native.revision.get(),
            description: "Input delivered. Take a fresh snapshot to inspect its effect.".into(),
            png: None,
        })
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded window pixel coordinates"
    )]
    fn native_input(&mut self, action: NativeAction) -> Result<(), String> {
        match action {
            NativeAction::Click { x, y, .. } => {
                let [width, height] = self.frame.as_ref().ok_or("The window closed.")?.size();
                if x >= width || y >= height {
                    return Err("Click is outside the snapshot.".into());
                }
                self.pointer_moved([x as f32, y as f32]);
                // Don't invoke desktop dragging, window buttons or window focus.
                if matches!(
                    self.hover,
                    super::Hit::Button(_) | super::Hit::Resize(_) | super::Hit::TopBar
                ) {
                    return Err("Native control does not operate desktop window controls.".into());
                }
                self.clicked(MouseButton::Left);
                self.released(MouseButton::Left);
            }
            NativeAction::Type { text, .. } => {
                if text.len() > 16 * 1024 {
                    return Err("Text is too long.".into());
                }
                self.type_text(&text);
            }
            NativeAction::Key { key, .. } => {
                let chords = crate::keymap::parse_keys(&key)?;
                if chords.len() != 1 {
                    return Err("Send one key chord per snapshot.".into());
                }
                let kept = chords[0].kept();
                let press = kept.press();
                let modifiers = self.modifiers;
                self.modifiers = press.modifiers;
                self.key_event(&press, false);
                self.modifiers = modifiers;
            }
            NativeAction::Snapshot => {}
        }
        Ok(())
    }
}

fn target_at(controls: &serde_json::Value, point: [f64; 2]) -> Option<&serde_json::Value> {
    controls.as_array()?.iter().rev().find(|control| {
        let Some(b) = control["bounds"].as_array() else {
            return false;
        };
        let values: Option<Vec<_>> = b.iter().map(serde_json::Value::as_f64).collect();
        let Some(b) = values.filter(|b| b.len() == 4) else {
            return false;
        };
        point[0] >= b[0] && point[1] >= b[1] && point[0] < b[0] + b[2] && point[1] < b[1] + b[3]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let runtime = clackatoo_daemon::RuntimeDir::at("/unused-native-test".into());
        let session = crate::session::Session::waiting(
            &runtime,
            crate::session::Proxy::Channel(std::sync::mpsc::channel().0),
        );
        let mut app = App::new(std::time::Instant::now(), None, session, String::new());
        app.notice = None;
        app.native.focus = "None".into();
        app
    }

    #[test]
    fn window_independently_refuses_ungranted_inspect_only_and_stale_actions() {
        let mut app = app();
        let chat = TerminalId(1);
        let key = NativeAction::Key {
            revision: 0,
            key: "<C-?>".into(),
        };
        assert!(
            app.native_act(chat, key.clone())
                .unwrap_err()
                .contains("not granted")
        );
        app.native.grant = Some((chat, NativeAccess::Inspect));
        assert!(
            app.native_act(chat, key.clone())
                .unwrap_err()
                .contains("Inspect only")
        );
        app.native.grant = Some((chat, NativeAccess::Control));
        assert!(
            app.native_act(chat, key.clone())
                .unwrap_err()
                .contains("stale")
        );
        app.native.snapshot = Some(0);
        app.native_act(chat, key.clone()).unwrap();
        assert!(
            app.shortcut_help.is_some(),
            "input follows the real application route"
        );
        assert!(app.native_act(chat, key).unwrap_err().contains("stale"));
    }

    #[test]
    fn automated_input_cannot_change_authority_or_answer_approvals() {
        assert!(changes_authority(&ClientMessage::NativeGrant {
            chat: TerminalId(1),
            access: NativeAccess::Control
        }));
        assert!(changes_authority(&ClientMessage::Request(Request::Chat {
            terminal: TerminalId(1),
            action: ChatAction::Answer {
                question: 2,
                option: Some("allow".into())
            }
        })));
        assert!(changes_authority(&ClientMessage::Request(Request::Chat {
            terminal: TerminalId(1),
            action: ChatAction::Set {
                setting: "tools".into(),
                value: "allow".into()
            }
        })));
    }

    #[test]
    fn a_moved_or_replaced_click_target_is_not_the_observed_target() {
        let before = serde_json::json!([{"role":"control","label":"Send","bounds":[10,20,30,40]}]);
        let after = serde_json::json!([{"role":"control","label":"Allow","bounds":[10,20,30,40]}]);
        assert_ne!(
            target_at(&before, [15.0, 25.0]),
            target_at(&after, [15.0, 25.0])
        );
        assert_eq!(target_at(&before, [50.0, 25.0]), None);
    }
}
