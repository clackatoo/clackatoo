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

use clackatoo_protocol::{
    Content, NewTerminal, PaneId, PaneTerminal, Request, Side, TerminalId, ViewUpdate, wire,
};

use super::{Asked, Desk};
use crate::chat::ChatPane;
use crate::finder::{self, Finder, LocalItem};
use crate::input::KeyPress;
use crate::pane::Pane;

/// What the bar says of keys that waited too long for a chat.
pub(super) const CHAT_WAITED: &str =
    "the keys typed waited too long for the chat: they and what they waited for were dropped";

/// What the bar says of keys that waited too long for the daemon to answer
/// a terminal: they go on without it.
pub(super) const TERMINAL_WAITED: &str =
    "the daemon took too long to answer the terminal: the keys typed went on without it";

/// What the bar says of a macro, with no keys typed behind it, that waited
/// too long for the daemon to answer a terminal.
pub(super) const MACRO_WAITED: &str =
    "the macro was stopped: the daemon took too long to answer the terminal";

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
        let chat = self.chats.get(&terminal)?;
        // The band over a subagent's chat goes to the chat that started
        // it, while that's open.
        if pixel[1] < 0.0 && self.band_height(terminal) > 0.0 {
            let open = self.subagent_band(terminal).is_some_and(|band| band.open);
            return Some(if open {
                crate::chat::Pointer::Hand
            } else {
                crate::chat::Pointer::Arrow
            });
        }
        Some(chat.pointer(pixel))
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
            let (mut copied, mut yanked, mut commands) = (Vec::new(), Vec::new(), Vec::new());
            let (mut wanting, mut opening) = (Vec::new(), Vec::new());
            let mut follows = Vec::new();
            for (&terminal, chat) in &mut self.chats {
                redraw |= chat.settle(fonts);
                follows.extend(chat.take_follows().into_iter().map(|id| (terminal, id)));
                copied.extend(chat.take_copies());
                self.opened.extend(chat.take_opened());
                yanked.extend(chat.take_yanks());
                commands.extend(chat.take_commands());
                if chat.view_mut().take_wants_files() {
                    wanting.push(terminal);
                }
                if let Some(target) = chat.view_mut().take_opening() {
                    // From where it works, made absolute if the daemon
                    // says, so it can go to an editor already open.
                    let view = chat.view();
                    let cwd = (view.paths().map(|paths| paths.cwd.as_str()))
                        .or_else(|| view.folder())
                        .map(str::to_owned);
                    opening.push((terminal, target, cwd));
                }
            }
            // What a chat's `@` menu offers, and what `gf` on a mention
            // opens (see `chat::mentions`).
            for terminal in wanting {
                self.ask_chat_files(terminal);
            }
            for (terminal, target, folder) in opening {
                self.open_link_of(terminal, target, folder.as_deref());
            }
            // A click on a task's line on a chat's shelf.
            for (chat, id) in follows {
                self.follow_task(chat, &id);
                redraw = true;
            }
            // What a chat's `/clear` or `/diff` is for, as the window
            // does it (see `chat::slash`).
            for command in commands {
                self.run(command);
                redraw = true;
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

    /// `:Ask`, and `<Space>a`: the focused editor's file, or the lines
    /// `lines` of it (from 0), go to the chat beside it, or the one last
    /// used in the tab, as a mention at the end of the message being typed
    /// (see `chat::mentions`), and the chat takes the focus, in insert
    /// mode, to say what to do with them. The daemon sends the lines' text
    /// with the message, as they are when it's sent.
    pub(super) fn ask_chat(&mut self, lines: Option<(u64, u64)>) {
        let Some((editor, copy)) = self.focused_editor() else {
            self.refuse("this pane has no file to ask a chat about".into());
            return;
        };
        let Some(shown) = self.shown else { return };
        let path = copy.path().to_owned();
        if path.is_empty() {
            self.refuse("this buffer has no file yet: write it first".into());
            return;
        }
        // A selection made with the keys, or the one `:'<,'>` was typed
        // from.
        let lines = editor.visual_lines().or(lines);
        let chat_of = |desk: &Self, pane: PaneId| -> Option<TerminalId> {
            let (_, _, found) = desk.workspaces.find_pane(pane)?;
            let terminal = found.content.terminal()?;
            desk.chats.contains_key(&terminal).then_some(terminal)
        };
        let Some(pane) = self.recent_in_tab(shown.pane, |desk, pane| chat_of(desk, pane).is_some())
        else {
            self.refuse("there's no chat in this tab to ask: Ctrl+B a opens one".into());
            return;
        };
        let Some(terminal) = chat_of(self, pane) else {
            return;
        };
        let Some(chat) = self.chats.get_mut(&terminal) else {
            return;
        };
        let written = crate::chat::mentions::from_chat_paths(&path, chat.view().paths());
        let lines = lines.map(|(first, last)| {
            let line = |line: u64| u32::try_from(line + 1).unwrap_or(u32::MAX);
            (line(first), line(last))
        });
        chat.view_mut()
            .append(format!("{} ", clackatoo_protocol::mention(&written, lines)));
        if let Some(editor) = self.focused_editor_mut() {
            editor.leave_visual();
        }
        self.show_pane(pane);
        if let Some(chat) = self.chats.get_mut(&terminal) {
            chat.enter_insert_mode();
            self.mode = super::WindowMode::Insert;
        }
    }

    /// `:nohlsearch`: the focused pane, a chat, an editor, a terminal or a
    /// list,
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
        } else if let Some(list) = self.focused_list_mut() {
            list.unlight();
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

    /// Starts the patience of keys waiting for a chat or a terminal again
    /// from `now`, if they wait, as what they wait for has asked again.
    pub(super) fn note_pane_asked_again(&mut self, now: std::time::Instant) {
        if let Some((since, _)) = &mut self.pane_wait {
            *since = now;
        }
    }

    /// The terminal of the pane keys wait for, if they wait.
    pub(super) fn waited_terminal(&self) -> Option<TerminalId> {
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
        let terminal = self
            .waited_terminal()
            .is_some_and(|terminal| self.panes.contains_key(&terminal));
        Some(
            since
                + if terminal {
                    super::macros::PATIENCE
                } else {
                    crate::editor::PATIENCE
                },
        )
    }

    /// Whether the keys waiting wait for a terminal's answer, not a chat.
    pub(super) fn waits_for_terminal(&self) -> bool {
        self.waited_terminal()
            .is_some_and(|terminal| self.panes.contains_key(&terminal))
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
    /// - Esc pressed drops the keys held and what a chat queued. (A
    ///   terminal's wait isn't given up by Esc: see `hold_while_waiting`.)
    /// - The focus leaving drops the keys, typed for that pane, and what
    ///   they waited for: a terminal's answer, when it comes, moves nothing.
    /// - Waiting too long drops a chat's keys and what they waited for, as
    ///   the bar says. A terminal's keys, which may be Ctrl+C for a program
    ///   after `i`, go on without the answer, which moves nothing when it
    ///   comes, in turn with what a binding among them types, once
    ///   [`type_macros`](Self::type_macros) hands them over; and the bar says
    ///   so once they have.
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
                pane.drop_waiting();
                go_on = how == GiveUp::TooLong;
            }
        }
        if go_on {
            let said = if self.holding() {
                TERMINAL_WAITED
            } else {
                MACRO_WAITED
            };
            self.stop_macro(None);
            self.say_after_held(said.to_owned());
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

    /// `:rewind`: the focused chat rewinds to before turn `number`, as
    /// people count them, or its last; with `force`, undoing files
    /// without asking.
    pub(super) fn rewind_chat(&mut self, number: Option<usize>, force: bool) {
        let asked = (self.focused_chat_mut()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            |chat| chat.view_mut().rewind_numbered(number, force),
        );
        if let Err(why) = asked {
            self.refuse(why);
        }
    }

    /// `:branch N`: the focused chat shows branch `to` where its branches
    /// part at the turn under its cursor, or the nearest above.
    pub(super) fn switch_chat_branch(&mut self, to: u16) {
        let asked = (self.focused_chat_mut()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            |chat| chat.view_mut().branch_numbered(to),
        );
        if let Err(why) = asked {
            self.refuse(why);
        }
    }

    /// `:branches`: the finder over every branch of the focused chat,
    /// where each parts from the others: Enter shows the one picked.
    pub(super) fn list_branches(&mut self) {
        let Some(chat) = self.focused_chat() else {
            self.refuse("that's for a chat with an agent".into());
            return;
        };
        let items = chat.view().branch_items();
        if items.is_empty() {
            self.refuse("this chat's conversation has no other branches".into());
            return;
        }
        self.close_finder();
        let case = crate::options::Case::of(&self.options);
        self.finder = Some(Finder::local(finder::Kind::Branches, items, case));
    }

    /// Shows the branch picked in the finder, `picked` as
    /// [`ChatView::branch_items`] names it, in the focused chat.
    pub(super) fn show_picked_branch(&mut self, picked: &str) {
        let asked = (self.focused_chat_mut()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            |chat| chat.view_mut().switch_picked(picked),
        );
        if let Err(why) = asked {
            self.refuse(why);
        }
    }

    /// Asks the focused chat's agent to stop what it runs in the
    /// background on line `line` of the chat's shelf, or the newest
    /// running.
    pub(super) fn stop_task(&mut self, line: Option<usize>) {
        let asked = (self.focused_chat_mut()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            |chat| chat.stop_task(line),
        );
        if let Err(why) = asked {
            self.refuse(why);
        }
    }

    /// `:taskoutput`: follows what the focused chat's agent's background
    /// task on line `line` of the shelf printed, or the newest with
    /// output (see [`Self::follow_task`]).
    pub(super) fn task_output(&mut self, line: Option<usize>) {
        let asked = (self.focused_chat()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            |chat| Ok((chat.terminal(), chat.task_on_shelf(line)?)),
        );
        match asked {
            Ok((chat, id)) => self.follow_task(chat, &id),
            Err(why) => self.refuse(why),
        }
    }

    /// Opens what task `id` of chat `chat` printed in a new terminal below
    /// the chat's pane, following it as it grows: the daemon makes it on
    /// the chat's machine, where the agent writes it.
    pub(super) fn follow_task(&mut self, chat: TerminalId, id: &str) {
        if !self.needs_daemon("a background task's output", wire::TASK_OUTPUT) {
            return;
        }
        let tail = tail_program(self.remote);
        let follow = (self.chats.get(&chat)).map_or_else(
            || Err("that chat is no longer open".to_owned()),
            |pane| pane.follow_task(id, tail),
        );
        let command = match follow {
            Ok(command) => command,
            Err(why) => return self.refuse(why),
        };
        let pane = self.shown_tab().and_then(|tab| {
            (tab.layout.panes().iter())
                .find(|pane| pane.content == Content::Terminal(chat))
                .map(|pane| pane.id)
        });
        let Some(pane) = pane else {
            return self.refuse("the chat isn't in the tab shown".to_owned());
        };
        let terminal = PaneTerminal::New(NewTerminal {
            size: self.new_pane_size(Some(Side::Below)),
            command: Some(command),
            cwd: None,
            record_to: None,
        });
        let split = Request::Split {
            pane,
            side: Side::Below,
            terminal,
            on: None,
        };
        self.request(split, Asked::Pane);
    }

    /// Opens the focused chat's plan's shelf, or folds it.
    pub(super) fn toggle_plan(&mut self) {
        let toggled = (self.focused_chat_mut()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            super::super::chat::ChatPane::toggle_plan,
        );
        if let Err(why) = toggled {
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

    /// Opens the finder on the messages sent in the focused chat, the
    /// newest first, starting with `query` typed: Enter puts the one
    /// picked in the chat's box (see `chat::prompts`).
    pub(super) fn list_prompts(&mut self, query: &str) {
        let Some(chat) = self.focused_chat() else {
            self.refuse("only a chat has messages sent to search".into());
            return;
        };
        let items: Vec<LocalItem> = (chat.view().node.prompts.iter().rev())
            .map(|prompt| {
                let text = one_line(prompt);
                LocalItem {
                    name: prompt.clone(),
                    name_len: text.len(),
                    text,
                    preview: prompt.lines().take(PREVIEWED).map(str::to_owned).collect(),
                    alert: None,
                }
            })
            .collect();
        if items.is_empty() {
            self.refuse("nothing has been sent in this chat yet".into());
            return;
        }
        self.close_finder();
        let case = crate::options::Case::of(&self.options);
        let mut finder = Finder::local(finder::Kind::Prompts, items, case);
        if !query.is_empty() {
            finder.paste(query);
        }
        self.finder = Some(finder);
    }

    /// Tells the daemon what's typed in chats' boxes where typing has
    /// paused since it changed, or wherever it's changed if `now`, as the
    /// window closes. Returns whether it told it any.
    pub(crate) fn send_drafts(&mut self, now: bool) -> bool {
        let mut sent = false;
        for chat in self.chats.values_mut() {
            sent |= chat.send_draft(now);
        }
        sent
    }

    /// Puts `text`, a message sent before, picked in the finder, in at
    /// the caret in the focused chat's box, which goes to insert mode.
    pub(super) fn put_in_focused_chat(&mut self, text: String) {
        if let Some(chat) = self.focused_chat_mut() {
            chat.put_recalled(text);
        }
    }
}

/// The most lines of a message the finder previews.
const PREVIEWED: usize = 200;

/// `prompt` on one line, as the finder lists it: its lines joined by a
/// mark, cut short.
fn one_line(prompt: &str) -> String {
    const LONGEST: usize = 300;
    let mut text = String::new();
    for (index, line) in prompt
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        if index > 0 {
            text.push_str(" ⏎ ");
        }
        text.push_str(line.trim());
        if text.len() > LONGEST {
            text.truncate(text.floor_char_boundary(LONGEST));
            text.push('…');
            break;
        }
    }
    text
}

/// Where `tail` is, to follow a task's output with: on this machine, the
/// first of `/usr/bin/tail` and `/bin/tail` there is, so a `tail` earlier
/// on the `PATH` isn't run; for a daemon on another machine, or with
/// neither here, the name, which that daemon looks up on its `PATH`.
pub(super) fn tail_program(remote: bool) -> &'static str {
    const KNOWN: [&str; 2] = ["/usr/bin/tail", "/bin/tail"];
    if remote {
        return "tail";
    }
    (KNOWN.into_iter())
        .find(|path| std::path::Path::new(path).is_file())
        .unwrap_or("tail")
}
