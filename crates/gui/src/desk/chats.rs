//! The desk's chats: panes showing a chat with an agent (see
//! [`crate::chat::ChatPane`]).
//!
//! The daemon hosts a chat as a terminal, so a chat's pane shows a
//! [`Content::Terminal`], and is attached, sized and acknowledged as a
//! terminal's is, by its terminal. Which a terminal is, the window learns
//! only from its first view: every terminal's pane starts as a terminal's
//! (see [`crate::pane::Pane`]), and one whose view is a chat's is handed
//! to a chat's pane as it comes, which takes over its mode and the
//! requests it sent. So a terminal shown has a pane in
//! [`Desk::panes`](super::Desk) or in [`Desk::chats`](super::Desk), never
//! both.
//!
//! [`Content::Terminal`]: clackatoo_protocol::Content::Terminal

use clackatoo_protocol::{TerminalId, ViewUpdate, wire};

use super::Desk;
use crate::chat::ChatPane;
use crate::input::KeyPress;

impl Desk {
    /// The chat `terminal` is, if it's one.
    pub(crate) fn chat(&self, terminal: TerminalId) -> Option<&ChatPane> {
        self.chats.get(&terminal)
    }

    /// The same, to change.
    pub(crate) fn chat_mut(&mut self, terminal: TerminalId) -> Option<&mut ChatPane> {
        self.chats.get_mut(&terminal)
    }

    /// The focused pane's chat, if it shows one.
    pub(crate) fn focused_chat(&self) -> Option<&ChatPane> {
        self.chats.get(&self.focused_terminal()?)
    }

    /// The same, to change.
    pub(super) fn focused_chat_mut(&mut self) -> Option<&mut ChatPane> {
        let terminal = self.focused_terminal()?;
        self.chats.get_mut(&terminal)
    }

    /// Hands the pane of `terminal` to a chat's pane, made from `update`,
    /// if that's the terminal's first view and shows a chat. Returns the
    /// chat's pane, if it did.
    pub(super) fn take_over_chat(
        &mut self,
        terminal: TerminalId,
        update: &ViewUpdate,
    ) -> Option<&ChatPane> {
        let ViewUpdate::Snapshot(view) = update else {
            return None;
        };
        let node = view.chat()?;
        let pane = self.panes.remove(&terminal)?;
        let mut chat = ChatPane::taking_over(pane, view, node);
        chat.set_options(&self.options);
        Some(self.chats.entry(terminal).insert_entry(chat).into_mut())
    }

    /// The number of the first turn the chat `terminal` holds, if it's a
    /// chat, for attaching to it again from there.
    pub(super) fn first_chat_turn(&self, terminal: TerminalId) -> Option<u32> {
        self.chats.get(&terminal).map(ChatPane::first_turn)
    }

    /// How the mouse pointer looks at `point`, if it's over a chat, which
    /// says by what it drew there.
    pub(crate) fn chat_pointer(&self, point: [f32; 2]) -> Option<crate::chat::Pointer> {
        let (terminal, _, pixel) = self.cell_at(point, None)?;
        Some(self.chats.get(&terminal)?.pointer(pixel))
    }

    /// Passes what an input method is composing to the focused pane, if
    /// it's a chat being typed in, which shows it in its box. Returns
    /// whether it was: the window shows it over a terminal itself.
    pub(crate) fn compose_in_chat(
        &mut self,
        composing: Option<(String, Option<(usize, usize)>)>,
    ) -> bool {
        self.focused_chat_mut()
            .is_some_and(|chat| chat.compose(composing))
    }

    /// Carries out what was typed or clicked in chats, which needs the
    /// fonts, and takes what they copied, as other copies are taken: `"+`
    /// or `"*` holds it, and the window puts it on the clipboard. Returns
    /// whether the window must be redrawn.
    pub(crate) fn settle_chats(&mut self, fonts: &mut crate::fonts::Fonts) -> bool {
        let mut redraw = false;
        let mut copied = Vec::new();
        for chat in self.chats.values_mut() {
            redraw |= chat.settle(fonts);
            copied.extend(chat.take_copies());
        }
        for (text, to) in copied {
            self.copied(text, to);
        }
        redraw
    }

    /// When a chat must be drawn again by itself, as for its agent's
    /// seconds to count or an image to finish decoding: the soonest.
    /// Chats suppress agent timers while offline; local image work can finish.
    pub(crate) fn chat_wakes(&self) -> Option<std::time::Instant> {
        self.chats.values().filter_map(ChatPane::live).min()
    }

    /// Tells every chat the daemon has gone out of reach, if `offline`, or
    /// come back.
    pub(super) fn chats_offline(&mut self, offline: bool) {
        for chat in self.chats.values_mut() {
            chat.set_offline(offline);
        }
    }

    /// Whether `press` would go to the focused chat's question, as one of
    /// the keys that answer it (see [`ChatPane::answers_question`]), which
    /// a key held down mustn't repeat.
    pub(crate) fn answers_question(&self, press: &KeyPress<'_>) -> bool {
        if self.input.is_some() || self.finder.is_some() || self.start.is_some() {
            return false;
        }
        self.focused_chat()
            .is_some_and(|chat| chat.answers_question(press))
    }

    /// Drops the answer the focused chat armed, if it did, as a key that
    /// doesn't reach it is had elsewhere: only the number again, or Enter,
    /// in the chat, may give it.
    pub(super) fn disarm_focused_chat(&mut self) {
        if let Some(chat) = self.focused_chat_mut() {
            chat.disarm();
        }
    }

    /// `:agentundo` and `:agentredo`: the focused chat undoes turn `turn`,
    /// or redoes it if `redo`, `force`d with `!`.
    pub(super) fn agent_undo(&mut self, turn: Option<usize>, redo: bool, force: bool) {
        let asked = (self.focused_chat_mut()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            |chat| chat.undo_turn(turn, redo, force),
        );
        if let Err(why) = asked {
            self.refuse(why);
        }
    }

    /// Local image viewing owns input even when the daemon is offline.
    pub(crate) fn chat_modal_key(&mut self, press: &KeyPress<'_>) -> bool {
        self.focused_chat_mut()
            .is_some_and(|chat| chat.view_mut().chat_modal_key(press))
    }

    /// The chat an explicit clipboard paste goes to, unless a prompt owns input.
    pub(crate) fn image_paste_target(&self) -> Option<TerminalId> {
        if self.input.is_some() || self.finding() {
            return None;
        }
        self.focused_chat()
            .filter(|chat| !chat.searching_models() && !chat.viewing_modal())
            .map(ChatPane::terminal)
    }

    pub(crate) fn begin_image_paste(&mut self, id: TerminalId) -> bool {
        let Some(chat) = self.chats.get_mut(&id).map(ChatPane::view_mut) else {
            return false;
        };
        if chat.pasting || chat.viewing_modal() {
            return false;
        }
        chat.begin_paste();
        true
    }

    pub(crate) fn finish_image_paste(
        &mut self,
        id: TerminalId,
        result: Result<crate::clipboard_image::Pasted, String>,
    ) {
        let supported = self.daemon_has(wire::CHAT_IMAGES);
        let Some(chat) = self.chats.get_mut(&id).map(ChatPane::view_mut) else {
            return;
        };
        let result = if !supported && matches!(result, Ok(crate::clipboard_image::Pasted::Image(_)))
        {
            Err("Update the daemon to paste images into chats.".into())
        } else {
            result
        };
        chat.finish_paste(result);
    }
}
