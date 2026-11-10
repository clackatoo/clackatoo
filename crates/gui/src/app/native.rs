//! A live window independently checks every daemon-routed automation request.
use std::cell::Cell;

use clackatoo_protocol::{
    BackgroundAction, ChatAction, ClientMessage, NativeAccess, NativeAction, NativeReply,
    PaneTerminal, Request, ReviewAction, TerminalId,
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

impl Native {
    /// Whether a chat has Control of this window.
    pub(super) fn controlled(&self) -> bool {
        matches!(self.grant, Some((_, NativeAccess::Control)))
    }
}

/// Whether `message`, sent by input an agent gave the window it was
/// granted for the chat `granted`, would do what only the user may: answer
/// a question or form, change a chat's settings, agent or access, run a
/// command (typed into a terminal, or one a new terminal or background
/// tab starts), start a chat, speak or type as the user to another, review
/// or comment on an agent's changes, post on a board, close or stop what
/// the user has open, or change what the daemon trusts (pairing, devices,
/// kept commands, other machines, installs and upgrades). The agent's own
/// tools do those, asking first where they must.
///
/// A speed bump, not a boundary: input that edits files, as Control's
/// may, can change what runs later, a config or a script.
pub(super) fn changes_authority(message: &ClientMessage, granted: Option<TerminalId>) -> bool {
    let request = match message {
        ClientMessage::NativeGrant { .. }
        | ClientMessage::Login { .. }
        | ClientMessage::Upgrade { .. }
        | ClientMessage::Board { .. } => return true,
        ClientMessage::Request(request) => request,
        _ => return false,
    };
    match request {
        Request::Chat { terminal, action } => match action {
            ChatAction::Answer { .. }
            | ChatAction::Fill { .. }
            | ChatAction::Set { .. }
            | ChatAction::Switch { .. }
            | ChatAction::SendTo { .. }
            | ChatAction::Fork { .. }
            // Opens a terminal running a login, or a new chat.
            | ChatAction::Remedy { .. }
            | ChatAction::Submit {
                target: Some(_), ..
            } => true,
            // What the user says or does in another chat, as the user.
            ChatAction::Send(_)
            | ChatAction::SendImages { .. }
            | ChatAction::Submit { .. }
            | ChatAction::SendQueued { .. }
            | ChatAction::Resend
            | ChatAction::Undo { .. }
            | ChatAction::Redo { .. }
            | ChatAction::Rewind { .. }
            | ChatAction::SwitchBranch { .. }
            | ChatAction::Draft { .. }
            | ChatAction::TakeBack { .. }
            | ChatAction::RemoveQueued { .. }
            | ChatAction::Stop
            | ChatAction::StopTask { .. } => Some(*terminal) != granted,
            _ => false,
        },
        Request::Review(action) => !matches!(action, ReviewAction::Refresh(_)),
        Request::NewTerminal(new) => new.command.is_some(),
        Request::NewWorkspace { terminal, .. }
        | Request::NewTab { terminal, .. }
        | Request::Split { terminal, .. }
        | Request::SplitTab { terminal, .. }
        | Request::Lend { terminal, .. } => starts(terminal),
        Request::Background(action) => matches!(
            action,
            BackgroundAction::Start { .. } | BackgroundAction::Dismiss { .. }
        ),
        Request::SendInput { .. }
        | Request::ClosePane { .. }
        | Request::CloseTab { .. }
        | Request::CloseWorkspace { .. }
        | Request::CloseTerminal { .. }
        | Request::AnswerPairing { .. }
        | Request::UnlockPairing
        | Request::RevokeDevice { .. }
        | Request::Keep { .. }
        | Request::KeepHistory { .. }
        // Showing what a program hid, which agents may then see.
        | Request::Unmask { .. }
        | Request::Connect { .. }
        | Request::Disconnect { .. }
        | Request::Forget { .. }
        | Request::Install { .. }
        | Request::Trust {
            action: clackatoo_protocol::TrustAction::Trust,
            ..
        }
        // The daemon's config, which says what runs (CLACKA-237).
        | Request::SetDaemonOption { .. }
        | Request::Forward(_) => true,
        _ => false,
    }
}

/// Whether a new pane showing `terminal` starts something that runs: a
/// terminal with a command, or a chat, whose agent may.
fn starts(terminal: &PaneTerminal) -> bool {
    match terminal {
        PaneTerminal::New(new) => new.command.is_some(),
        PaneTerminal::Agent(_) => true,
        _ => false,
    }
}

impl Native {
    /// What the top bar's badge says of the access a chat holds to this
    /// window, if one does.
    pub(super) fn grant_badge(&self) -> Option<String> {
        let (chat, access) = self
            .grant
            .filter(|(_, access)| *access != NativeAccess::Off)?;
        Some(format!("Agent {access:?} · chat {chat}"))
    }
}

impl App {
    /// A click on the grant's badge: shows the chat that holds the access.
    /// Never for the agent's own clicks.
    pub(super) fn go_to_granted_chat(&mut self) {
        if let Some((chat, _)) = self.native.grant.filter(|_| !self.native.automating) {
            self.desk.go_to_chat(chat);
        }
    }

    /// A click on the `×` of the grant's badge: revokes the access at once
    /// in this window, then tells the daemon. Never for the agent's own
    /// clicks.
    pub(super) fn revoke_native(&mut self) {
        let Some((chat, _)) = self.native.grant.filter(|_| !self.native.automating) else {
            return;
        };
        self.native.grant = None;
        self.native.snapshot = None;
        self.native.pending = Some((chat, NativeAccess::Off));
        if let Some(sender) = &mut self.sender {
            let _ = sender.send(&ClientMessage::NativeGrant {
                chat,
                access: NativeAccess::Off,
            });
        }
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
            return self.native_snapshot(access);
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
        self.desk.automating = true;
        self.native.refused = false;
        let result = self.native_input(action);
        // Nothing it typed goes on once these checks are off: a macro or
        // binding waiting for an answer, or keys held behind one.
        let dropped = self.desk.drop_deferred_keys();
        self.send_outbox();
        self.native.automating = false;
        self.desk.automating = false;
        self.request_redraw();
        result?;
        if self.native.refused {
            return Err("Automated input cannot grant access, change permissions, answer approval questions or forms, type into terminals, start commands, send messages to other chats as the user, or open external windows.".into());
        }
        let description = if dropped {
            "Input delivered, but keys left waiting for an answer, as a macro's rest, were dropped. Take a fresh snapshot to inspect its effect."
        } else {
            "Input delivered. Take a fresh snapshot to inspect its effect."
        };
        Ok(NativeReply {
            revision: self.native.revision.get(),
            description: description.into(),
            png: None,
        })
    }

    /// Draws the window for a snapshot the chat granted `access` asked for.
    fn native_snapshot(&mut self, access: NativeAccess) -> Result<NativeReply, String> {
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
        Ok(NativeReply {
            revision,
            description,
            png: Some(image.data),
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
                // Nor the badge of its own access.
                if matches!(self.hover, super::Hit::Grant | super::Hit::Revoke) {
                    return Err("Native control does not operate the access badge.".into());
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
        let mut app = App::new(
            std::time::Instant::now(),
            None,
            session,
            crate::app::host::Spec::default(),
            crate::app::host::Link::default(),
        );
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
        let granted = Some(TerminalId(1));
        let changes = |message: &ClientMessage| changes_authority(message, granted);
        assert!(changes(&ClientMessage::NativeGrant {
            chat: TerminalId(1),
            access: NativeAccess::Control
        }));
        let chat = |terminal, action| ClientMessage::Request(Request::Chat { terminal, action });
        let answer = ChatAction::Answer {
            question: 2,
            option: Some("allow".into()),
        };
        assert!(changes(&chat(TerminalId(1), answer)));
        let set = ChatAction::Set {
            setting: "tools".into(),
            value: "allow".into(),
        };
        assert!(changes(&chat(TerminalId(1), set)));
        // Nor saves the daemon's settings.
        assert!(changes(&ClientMessage::Request(Request::SetDaemonOption {
            name: "subagents.working".into(),
            value: Some("64".into()),
        })));
        // It types in its own chat, but speaks for the user in no other.
        let send = ChatAction::Send("go".into());
        assert!(!changes(&chat(TerminalId(1), send.clone())));
        assert!(changes(&chat(TerminalId(2), send)));
        let draft = ChatAction::Draft { text: "x".into() };
        assert!(!changes(&chat(TerminalId(1), draft)));
        // Nor shows itself what a program hid.
        assert!(changes(&ClientMessage::Request(Request::Unmask {
            terminal: TerminalId(3),
            line: Some(4),
        })));
        // It runs no command, by typing into a terminal or starting one.
        assert!(changes(&ClientMessage::Request(Request::SendInput {
            terminal: TerminalId(3),
            bytes: b"rm -rf ~\r".to_vec(),
        })));
        let terminal = |command| {
            ClientMessage::Request(Request::NewTerminal(clackatoo_protocol::NewTerminal {
                size: clackatoo_protocol::Size::new(80, 24).unwrap(),
                command,
                cwd: None,
                record_to: None,
            }))
        };
        assert!(changes(&terminal(Some(vec!["sh".into()]))));
        assert!(!changes(&terminal(None)), "a shell runs nothing typed");
        assert!(changes(&ClientMessage::Request(Request::UnlockPairing)));
        assert!(!changes(&ClientMessage::Request(Request::ListTerminals)));
        // Not its own chat's agent, nor a provider, changed either.
        let fork = ChatAction::Fork {
            keep: 1,
            place: clackatoo_protocol::ForkPlace::Tab,
        };
        assert!(changes(&chat(TerminalId(1), fork)));
        // Nor its changes reviewed, nor comments sent as the user.
        let send = ClientMessage::Request(Request::Review(ReviewAction::Send(TerminalId(1))));
        assert!(changes(&send));
        // Nor the user's panes closed, nor another chat's draft or turn.
        let close = Request::ClosePane {
            pane: clackatoo_protocol::PaneId(4),
        };
        assert!(changes(&ClientMessage::Request(close)));
        assert!(changes(&chat(TerminalId(2), ChatAction::Stop)));
        let draft = ChatAction::Draft { text: "x".into() };
        assert!(changes(&chat(TerminalId(2), draft)));
        // Nor trust a folder to run its code; taking trust away is fine.
        let trust = |action| {
            ClientMessage::Request(Request::Trust {
                action,
                folder: None,
                buffer: None,
            })
        };
        assert!(changes(&trust(clackatoo_protocol::TrustAction::Trust)));
        assert!(!changes(&trust(clackatoo_protocol::TrustAction::Untrust)));
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
