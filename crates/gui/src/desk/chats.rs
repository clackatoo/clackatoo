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
use crate::pane::Pane;

/// What the bar says of keys that waited too long for a chat.
pub(super) const CHAT_WAITED: &str =
    "the keys typed waited too long for the chat: they and what they waited for were dropped";

/// What the bar says of keys that waited too long for the daemon to answer
/// a terminal's search: they go on without it.
pub(super) const TERMINAL_WAITED: &str =
    "the daemon took too long to answer the search: the keys typed after it went on without it";

/// How a wait for a chat or a terminal is given up (see
/// [`Desk::give_up_pane_wait`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GiveUp {
    /// Esc pressed.
    Cancelled,
    /// The focus left the pane waited in.
    FocusMoved,
    /// It waited past its patience.
    TooLong,
}

/// The most rounds of settling chats and typing the keys that waited for
/// them [`Desk::settle_chats`] goes at once, so one call stays short: a
/// binding typing more yanks and searches than that reaches it, and what's
/// left of it waits, held, for the next, which the window makes before it
/// draws, and goes on.
const MOST_ROUNDS: usize = 4096;

#[cfg(test)]
thread_local! {
    /// How many rounds [`Desk::settle_chats`] has gone, for tests.
    pub(super) static SETTLE_ROUNDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
        // A terminal is a chat or not for good: one whose screen has come
        // stays a terminal, whatever a confused daemon says after.
        if self.panes.get(&terminal)?.screen().is_some() {
            log::warn!("terminal {terminal} sent a chat's view after a screen: ignored");
            return None;
        }
        let pane = self.panes.remove(&terminal)?;
        let mut chat = ChatPane::taking_over(pane, view, node);
        chat.view_mut().share_finds(&self.finds);
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
    ///
    /// Keys typed after a chat's yank or search waited for it (see
    /// [`pane_waits_to_settle`](Self::pane_waits_to_settle)): they go on
    /// now, and if one yanks or searches again, it's settled in turn.
    pub(crate) fn settle_chats(&mut self, fonts: &mut crate::fonts::Fonts) -> bool {
        let mut redraw = false;
        // Bounded too, though each round takes a key or ends.
        for _ in 0..MOST_ROUNDS {
            #[cfg(test)]
            SETTLE_ROUNDS.with(|rounds| rounds.set(rounds.get() + 1));
            let (mut copied, mut yanked) = (Vec::new(), Vec::new());
            for chat in self.chats.values_mut() {
                redraw |= chat.settle(fonts);
                copied.extend(chat.take_copies());
                yanked.extend(chat.take_yanks());
            }
            for (text, to) in copied {
                self.copied(text, to);
            }
            // Into the registers, as a terminal's yanks go.
            for yank in yanked {
                self.pane_yanked(yank);
            }
            // A search that found nothing stops a macro or a binding, as
            // Vim's stops on a failing command.
            if self.take_pane_failed() && self.frames_typing() {
                self.stop_macro(None);
            }
            // Still waiting, as behind a paste reading the clipboard: the
            // keys stay held, for the settling once it's read.
            if self.pane_waits_to_settle() {
                return redraw;
            }
            self.pane_wait = None;
            if !self.holding() && !self.frames_typing() {
                break;
            }
            // Each time round takes at least the key that waits, so this
            // ends.
            redraw |= self.type_macros();
            if !self.pane_waits_to_settle() {
                break;
            }
        }
        redraw
    }

    /// `:nohlsearch`: the focused pane, a chat, an editor or a terminal,
    /// stops lighting up what its last search matches, the match found
    /// and the rest, until the next search, `n` or `N`.
    pub(super) fn stop_highlighting(&mut self) {
        if let Some(chat) = self.focused_chat_mut() {
            chat.view_mut().stop_highlighting();
        } else if let Some(shown) = self.shown
            && let Some(editor) = self.editors.panes.get_mut(&shown.pane)
        {
            editor.forget_match();
        } else if let Some(pane) = self.focused_pane() {
            pane.forget_match();
        }
    }

    /// Whether a chat's or a terminal's command failed since this was
    /// last asked.
    pub(super) fn take_pane_failed(&mut self) -> bool {
        let chats =
            (self.chats.values_mut()).fold(false, |failed, chat| chat.take_failed() | failed);
        (self.panes.values_mut()).fold(chats, |failed, pane| pane.take_failed() | failed)
    }

    /// Notes that keys wait for the focused chat or terminal from `now`,
    /// if they didn't already.
    pub(super) fn note_pane_wait(&mut self, now: std::time::Instant) {
        if let Some(shown) = self.shown {
            self.pane_wait.get_or_insert((now, shown.pane));
        }
    }

    /// The terminal of the pane keys wait for, if they wait.
    fn waited_terminal(&self) -> Option<TerminalId> {
        let (_, pane) = self.pane_wait?;
        let (_, _, found) = self.workspaces.find_pane(pane)?;
        found.content.terminal()
    }

    /// When keys waiting for a chat or a terminal give up, if they wait: a
    /// chat's yank or search waits only for the window, behind a paste
    /// reading the clipboard, so [`editor::PATIENCE`]; a terminal's search
    /// for the daemon, maybe across machines, so as long as a macro waits
    /// for it.
    ///
    /// [`editor::PATIENCE`]: crate::editor::PATIENCE
    pub(super) fn pane_wait_gives_up(&self) -> Option<std::time::Instant> {
        let (since, _) = self.pane_wait?;
        let chat = self
            .waited_terminal()
            .is_some_and(|terminal| self.chats.contains_key(&terminal));
        Some(
            since
                + if chat {
                    crate::editor::PATIENCE
                } else {
                    super::macros::PATIENCE
                },
        )
    }

    /// Whether keys have waited for a chat or a terminal past its patience
    /// by `now` (see [`pane_wait_gives_up`](Self::pane_wait_gives_up)).
    pub(super) fn pane_waited_too_long(&self, now: std::time::Instant) -> bool {
        self.pane_wait_gives_up().is_some_and(|when| now >= when)
    }

    /// Gives up on what keys wait for in the chat or the terminal they wait
    /// in, as `how` says, and stops the macro typing them, so nothing waits
    /// any more. Returns whether the keys held go on, rather than being
    /// dropped.
    ///
    /// - Esc pressed drops the keys held and what a chat queued; a
    ///   terminal's search or block, already asked, still lands, as Vim's
    ///   `/foo<CR>` does before the Esc after it.
    /// - The focus leaving drops the keys, typed for that pane, and what
    ///   they waited for: a terminal's answer, when it comes, moves nothing.
    /// - Waiting too long drops a chat's keys and what they waited for, as
    ///   the bar says. A terminal's keys, which may be Ctrl+C for a program
    ///   after `i`, go on now without the answer, which moves nothing when
    ///   it comes, and the bar says so.
    pub(super) fn give_up_pane_wait(&mut self, how: GiveUp) -> bool {
        let waited = self.waited_terminal();
        self.pane_wait = None;
        let mut said = None;
        let mut go_on = false;
        if let Some(terminal) = waited {
            if let Some(chat) = self.chats.get_mut(&terminal) {
                chat.view_mut().drop_reading();
                said = (how == GiveUp::TooLong).then_some(CHAT_WAITED);
            } else if let Some(pane) = self.panes.get_mut(&terminal) {
                if how != GiveUp::Cancelled {
                    pane.drop_waiting();
                }
                go_on = how == GiveUp::TooLong;
                said = go_on.then_some(TERMINAL_WAITED);
            }
        }
        if go_on {
            self.stop_macro(None);
            // In the order they were pressed, now; those behind a search
            // one of them asks wait for that.
            while !self.pane_waits_to_settle() && self.have_held() {}
        } else {
            self.gave_up();
        }
        if let Some(said) = said {
            self.say(said.to_owned());
        }
        go_on
    }

    /// Whether the focused chat has a yank or a search waiting for the
    /// fonts to be carried out, which moves its cursor or fills a
    /// register, or the focused terminal a search or a block waiting for
    /// the daemon's answer: the keys after it wait for it, as they wait for
    /// an editor's grammar.
    pub(super) fn pane_waits_to_settle(&self) -> bool {
        let Some(terminal) = self.focused_terminal() else {
            return false;
        };
        match self.chats.get(&terminal) {
            Some(chat) => chat.reading_unsettled(),
            None => (self.panes.get(&terminal)).is_some_and(Pane::waits_for_answer),
        }
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
