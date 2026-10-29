//! Reading a chat in normal mode: a cursor in the conversation's text,
//! moved as in Vim, visual mode to select, `y` to yank (see
//! [`super::yank`]), `/` and `?` to search (see [`super::search`]), and
//! `z` to open and fold turns' work.
//!
//! The text is laid out in proportional fonts, so the cursor isn't on a
//! grid of cells, as the cursor of terminals and editors is (see
//! [`crate::motion`]): it's a place in a block of text, and moving up and
//! down goes by the lines the text was laid out in, keeping as near the
//! same distance across as it can. Moving off the end of a block goes on
//! to the next, as the conversation shows them. Keys move it at once, from
//! what was laid out last; what they copy waits for the fonts, as the turns
//! it takes in may need laying out.
//!
//! The keys are read by the grammar every pane reads them with, as
//! [`ChatKeys`] says a chat reads them, so a count goes before any move.
//!
//! | Keys | What they do |
//! |---|---|
//! | `h` `j` `k` `l`, arrows, `+` `-` | Move a character, or a line |
//! | `w` `b` `e`, `W` `B` `E` | Move by words, or by anything between blanks |
//! | `f` `t` `F` `T`, `;` `,` | To a character on the line, or just before it, and again |
//! | `0` `^` `$` | To the start of the line, its first word, its end |
//! | `{` `}` | To the block before, or after |
//! | `gg` `G` | To the start of the chat, or its end |
//! | Ctrl+D Ctrl+U, Ctrl+E Ctrl+Y, Ctrl+F and the pages | Scroll as far as in any pane, the cursor kept in view |
//! | `v` `V` | Select characters, or lines; `o` goes to the other end |
//! | `y` and a motion or object, `yy` `Y` | Yank what it moves over, or the object, into the registers: `yix` a block of code, `yim` a message or card, `yam` a turn |
//! | `"a`, before `y` | Yank into register `a`, or onto its end with `"A` |
//! | `v` then `y`, `Y` | Yank what's selected, or its lines |
//! | `/` `?`, `n` `N` | Search the chat's text forward or back, and again |
//! | `za` `zo` `zc`, Enter | Open or fold the tool, turn's work, or subagents under the cursor; `zR` and `zM` all turns and subagents |
//! | `zl` `zh`, `zL` `zH` | Scroll the table the cursor's in sideways, a little or half the column; Shift and the wheel do too |
//! | `u`, Ctrl+R | Undo what the turn changed in files, or redo it; with nothing there, the last turn's, or the first's undone |
//! | `K` | Show how much context the chat fills, and its account's limits, or hide it again |
//! | `i` `a` `I` `A` `o`, Enter | Type a message |
//! | `1`–`9`, then the same again or Enter | While the agent asks something, pick an answer, then give it; Esc leaves it be |

use clackatoo_protocol::{ChatAction, ChatNode, Item};
use winit::keyboard::{Key, NamedKey};

use super::card::Card;
use super::paint::{AGENTS, FOLD, How, ITEMS, PROMPT, Picked, Position, TextKey, control};
use super::{ChatView, Input};
use crate::input::KeyPress;
use crate::motion::Motion;
use crate::objects::Object;
use crate::vim::{
    Action, Command, Dialect, Operator, Parsed, Read, Reader, Scroll, Selection, Stroke, Target,
};

/// What `z` and a key fold or open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    /// `za`: the work, tool or card under the cursor, open or folded.
    Toggle,
    /// `zo`: it, opened.
    Open,
    /// `zc`: it, folded.
    Close,
    /// `zR`, or `zM` if not `open`: every turn's work and card.
    All { open: bool },
}

/// A command of a chat's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChatKey {
    /// `i`, `a`, `I` and `A`, in visual mode too: type a message.
    Compose,
    /// Enter: open or fold what's under the cursor, or type a message.
    Enter,
    /// Esc: stop selecting.
    Escape,
    /// `K`: the chat's context and limits, shown or hidden.
    Detail,
    /// `u`, or Ctrl+R if `redo`: the turn's changes to files.
    Undo {
        redo: bool,
    },
    Fold(Fold),
    /// `zl` and `zh`, `zL` and `zH` if `half`: the table the cursor's in,
    /// sideways.
    Sideways {
        right: bool,
        half: bool,
    },
    /// `R`: the last message, which failed, sent again.
    Resend,
    /// `gf` and `gx`: the file a message mentions under the cursor, or the
    /// link there, opened.
    OpenLink,
}

/// How a chat reads keys in normal and visual mode: the moves it has, by
/// its lines laid out in proportional fonts, with counts, `f` `t` `F` `T`
/// `;` `,` on a line, `v` `V` `o`, the scrolling keys as far as in any
/// pane, and the keys into insert mode, which type a message; `y` as
/// Vim's operator, with any of those motions or the objects it finds,
/// `ix` and `im` among them, and a register before it; `/`, `?`, `n` and
/// `N`; and its own: Enter, `K`, `u` and Ctrl+R for turns, and `z` for
/// folding and tables. It takes no mark, edit or other operator, and no
/// register before anything but a yank: a chat's text isn't the user's
/// to change.
pub(crate) struct ChatKeys;

impl Dialect for ChatKeys {
    type Own = ChatKey;

    fn describe(&self, action: ChatKey) -> String {
        match action {
            ChatKey::Compose => "Compose a message",
            ChatKey::Enter => "Open or fold item; otherwise compose",
            ChatKey::Escape => "Clear selection",
            ChatKey::Detail => "Open context and usage limits",
            ChatKey::Undo { redo: false } => "Undo turn file changes",
            ChatKey::Undo { redo: true } => "Redo turn file changes",
            ChatKey::Fold(Fold::Toggle) => "Toggle work, tool or subagents",
            ChatKey::Fold(Fold::Open) => "Expand work, tool or subagents",
            ChatKey::Fold(Fold::Close) => "Fold work, tool or subagents",
            ChatKey::Fold(Fold::All { open: true }) => "Expand all work",
            ChatKey::Fold(Fold::All { open: false }) => "Fold all work",
            ChatKey::Sideways {
                right: true,
                half: false,
            } => "Scroll table right",
            ChatKey::Sideways {
                right: false,
                half: false,
            } => "Scroll table left",
            ChatKey::Sideways {
                right: true,
                half: true,
            } => "Scroll table right half a view",
            ChatKey::Sideways {
                right: false,
                half: true,
            } => "Scroll table left half a view",
            ChatKey::Resend => "Send the failed message again",
            ChatKey::OpenLink => "Open file or link under cursor",
        }
        .into()
    }

    fn own(&self, stroke: Stroke, _keys: &mut Reader<'_>, visual: bool) -> Option<Read<ChatKey>> {
        let key = match stroke {
            Stroke::Enter => ChatKey::Enter,
            Stroke::Escape => ChatKey::Escape,
            Stroke::Char {
                ch: 'r',
                ctrl: true,
            } => ChatKey::Undo { redo: true },
            Stroke::Char { ch, ctrl: false } => match ch {
                'i' | 'a' | 'I' | 'A' => ChatKey::Compose,
                // In visual mode, `o` goes to the selection's other end.
                'o' if !visual => ChatKey::Compose,
                'O' if !visual => return Some(Read::No),
                'u' => ChatKey::Undo { redo: false },
                'K' if !visual => ChatKey::Detail,
                'R' if !visual => ChatKey::Resend,
                'K' => return Some(Read::No),
                _ => return None,
            },
            _ => return None,
        };
        Some(Read::Yes(key))
    }

    fn after_g(
        &self,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<ChatKey>> {
        matches!(stroke.plain()?, 'f' | 'x').then_some(Read::Yes(ChatKey::OpenLink))
    }

    fn after_z(
        &self,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<ChatKey>> {
        let key = match stroke.plain()? {
            'a' => ChatKey::Fold(Fold::Toggle),
            'o' => ChatKey::Fold(Fold::Open),
            'c' => ChatKey::Fold(Fold::Close),
            'R' => ChatKey::Fold(Fold::All { open: true }),
            'M' => ChatKey::Fold(Fold::All { open: false }),
            'l' | 'h' | 'L' | 'H' => {
                let ch = stroke.plain()?;
                ChatKey::Sideways {
                    right: ch.eq_ignore_ascii_case(&'l'),
                    half: ch.is_ascii_uppercase(),
                }
            }
            _ => return None,
        };
        Some(Read::Yes(key))
    }

    fn object(&self, key: char) -> Option<Object> {
        match key {
            // `x` for code, as `ic` is a class, as anywhere, which a chat
            // hasn't got.
            'x' => Some(Object::CodeBlock),
            'm' => Some(Object::Message),
            // As in Vim: `b` is `(`, and `B` is `{`.
            'b' => Some(Object::Bracket {
                open: '(',
                close: ')',
            }),
            'B' => Some(Object::Bracket {
                open: '{',
                close: '}',
            }),
            key => Object::of_key(key),
        }
    }

    fn takes(&self, command: &Command<ChatKey>) -> bool {
        match command.action {
            Action::Operate(Operator::Yank, target) => match target {
                Target::Motion(motion) => {
                    moves(motion) || matches!(motion, Motion::FindAgain { .. })
                }
                Target::Object { object, .. } => !matches!(object, Object::Block | Object::Code(_)),
                Target::Lines | Target::Selection | Target::SelectedLines => true,
                Target::Mark { .. } => false,
            },
            // A register is for a yank alone.
            _ if command.register.is_some() => false,
            Action::Move(motion) => moves(motion),
            Action::FindAgain { .. }
            | Action::Scroll(_)
            | Action::Visual { .. }
            | Action::OtherEnd
            | Action::Insert(_)
            | Action::Search { .. }
            | Action::SearchAgain { .. }
            | Action::Own(_) => true,
            _ => false,
        }
    }
}

/// Whether a chat moves by `motion`, as its cursor moves through its
/// lines laid out in proportional fonts (see [`ChatView::moved`]).
fn moves(motion: Motion) -> bool {
    matches!(
        motion,
        Motion::Down(_)
            | Motion::Up(_)
            | Motion::RowDown(_)
            | Motion::RowUp(_)
            | Motion::Left
            | Motion::Right
            | Motion::LeftOver
            | Motion::RightOver
            | Motion::WordStart
            | Motion::WordEnd
            | Motion::WordBack
            | Motion::BigWordStart
            | Motion::BigWordEnd
            | Motion::BigWordBack
            | Motion::LineStart
            | Motion::FirstNonBlank
            | Motion::LineEnd
            | Motion::NextLine
            | Motion::PreviousLine
            | Motion::Paragraph { .. }
            | Motion::First
            | Motion::Last
            | Motion::Find { .. }
    )
}

/// Which way to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Way {
    Back,
    On,
}

impl ChatView {
    /// What visual mode selects, while it does.
    pub(crate) fn visual(&self) -> Option<Selection> {
        match self.picked {
            Some(Picked {
                how: How::Visual(kind),
                ..
            }) => Some(kind),
            _ => None,
        }
    }

    /// Whether text selected with the mouse is what `y` would yank, with
    /// nothing typed but a count or a register: the bar says VISUAL.
    pub(crate) fn mouse_selecting(&self) -> bool {
        matches!(
            self.picked,
            Some(Picked {
                how: How::Mouse,
                ..
            })
        ) && crate::vim::only_count_and_register(self.keys.so_far())
    }

    /// The keys of a command half typed in normal or visual mode, such as
    /// `z` waiting for `a`, or a count.
    pub(crate) fn keys(&self) -> &crate::vim::Keys {
        &self.keys
    }

    /// Forgets what's half typed, as a binding of the window's took the
    /// key after a count.
    pub(crate) fn forget_keys(&mut self) {
        self.keys.clear();
    }

    /// Ends what was half done in reading as the focus leaves the chat: a
    /// visual selection, a two-key command half typed, and an answer
    /// armed. The cursor stays where it is.
    pub(crate) fn leave(&mut self) {
        self.give_up_search();
        self.leave_visual();
        self.keys.clear();
        self.armed = None;
    }

    /// The answer a number armed in normal mode, by its index, which the
    /// question's card picks out.
    pub(super) fn armed(&self) -> Option<usize> {
        self.armed.map(|(index, _)| index)
    }

    /// Drops the answer armed, if one is, as a click, the wheel or the
    /// window losing the focus does. Returns whether one was, so the card
    /// must be drawn again.
    pub(crate) fn disarm(&mut self) -> bool {
        self.armed.take().is_some()
    }

    /// A key in normal or visual mode. Returns whether the chat must be
    /// redrawn, and whether the key goes to insert mode, to type a
    /// message.
    pub(crate) fn read_key(&mut self, press: &KeyPress<'_>) -> (bool, bool) {
        if self.chat_modal_key(press) {
            return (true, false);
        }
        // A search being typed takes every key.
        if self.prompt_key(press) {
            return (true, false);
        }
        // What the window said of the chat lasts until the next key, as
        // does the meter's detail `K` showed, which `K` hides, and an
        // answer armed, which the key may give.
        if self.menu.is_some() {
            let mut settled = super::Settled::default();
            if self.menu_key(press.key, press.modifiers, &mut settled) {
                self.queued
                    .extend(settled.actions.into_iter().map(Input::Act));
                return (true, false);
            }
        }
        let unsaid = self.unsay();
        let detail = self.hide_detail();
        let disarmed = self.armed.is_some();
        // A form's keys come first: `j` and `k` move through it.
        if !self.keys.pending()
            && let Some(typing) = self.form_key(press, false)
        {
            return (true, typing);
        }
        // So do a plan's: `j` and `k` scroll it.
        if self.proposal_key(press) {
            return (true, false);
        }
        if self.question_key(press) {
            return (true, false);
        }
        let k = matches!(press.key, Key::Character(text) if text == "K");
        if detail && k && self.visual().is_none() {
            // A command half typed is over, as with any other key.
            self.keys.clear();
            return (true, false);
        }
        let (redraw, insert) = self.read(press);
        (redraw || unsaid || detail || disarmed, insert)
    }

    /// A key in normal or visual mode while the agent asks something. A
    /// number arms its answer, which the card picks out and the bar names,
    /// and the same number again, or Enter, gives it, if it comes in
    /// [`ARMED_FOR`](super::ARMED_FOR) after: one key mustn't answer, nor
    /// a count typed in one go, as Vim's habits press numbers for counts,
    /// and `2j` or `22j` would say "Always allow". Too soon, it stays
    /// armed; too late, it's armed again. Any other key drops what's
    /// armed, and reads as ever. Returns whether it took the key.
    fn question_key(&mut self, press: &KeyPress<'_>) -> bool {
        let armed = self.armed.take();
        let Some(question) = (self.node.question.as_ref()).filter(|q| q.form.is_none()) else {
            return false;
        };
        let modifiers = press.modifiers;
        if modifiers.ctrl || modifiers.alt || self.keys.pending() {
            return false;
        }
        let number = match press.key {
            Key::Character(text) => super::numbered(Some(text.as_str())),
            _ => None,
        };
        let enter = *press.key == Key::Named(NamedKey::Enter);
        let index = match (number, armed) {
            (Some(index), _) => index,
            (None, Some((armed, _))) if enter => armed,
            _ => return false,
        };
        let Some(choice) = question.choices.get(index) else {
            self.say(self.answer_hint());
            return true;
        };
        let (id, number) = (question.id, index + 1);
        let armed_said = format!("{number} {} · {number} again or Enter", choice.name);
        let now = self.now();
        // How long ago this answer was armed, if it was; or none, should
        // the clock have gone back.
        let since = armed
            .filter(|&(armed, _)| armed == index)
            .map(|(_, at)| now.checked_sub(at));
        match since {
            // Another, or none, armed; or this one armed so long ago, or
            // in a time gone back, that it's armed again.
            None | Some(None) => {
                self.armed = Some((index, now));
                self.say(armed_said);
            }
            Some(Some(since)) if since >= super::ARMED_FOR.end => {
                self.armed = Some((index, now));
                self.say(armed_said);
            }
            // Typed in one go with the number: it stays armed, as it was.
            Some(Some(since)) if since < super::ARMED_FOR.start => {
                self.armed = armed;
                self.say(armed_said);
            }
            Some(Some(_)) if self.just_asked() => {
                self.armed = armed;
                self.say(format!(
                    "a question just came: press {number} again or Enter to answer it"
                ));
            }
            Some(Some(_)) => self.queued.push(Input::Answer {
                question: id,
                pick: super::Pick::Choice(index),
            }),
        }
        true
    }

    /// A key in normal or visual mode, as [`read_key`](Self::read_key)
    /// says: read by the grammar every pane reads keys with, as
    /// [`ChatKeys`] says a chat reads them.
    fn read(&mut self, press: &KeyPress<'_>) -> (bool, bool) {
        // Ctrl+Shift+C copies what's selected, as in other panes,
        // whatever's half typed.
        let modifiers = press.modifiers;
        if modifiers.ctrl
            && modifiers.shift
            && matches!(press.key, Key::Character(text) if text.eq_ignore_ascii_case("c"))
        {
            self.keys.clear();
            self.yank_to_clipboard();
            return (true, false);
        }
        let visual = self.visual().is_some();
        let mut redraw = false;
        // Ctrl with Shift is Ctrl in a chat, as Ctrl+Shift+E scrolls as
        // Ctrl+E does: no program's there to tell them apart for.
        let strokes = crate::vim::strokes(press, false)
            .into_iter()
            .map(|stroke| match stroke {
                Stroke::Char { ch, ctrl: true } => Stroke::ctrl(ch.to_ascii_lowercase()),
                stroke => stroke,
            });
        for stroke in strokes {
            let was_pending = self.keys.pending();
            // Text selected with the mouse is a visual selection to Vim:
            // `y` yanks it, after a count or a register, which wait with
            // it. Any other key forgets it, as in a terminal.
            let mouse = self.mouse_selecting();
            let y = stroke == Stroke::char('y');
            let parsed = self.keys.read(stroke, visual || (mouse && y), &ChatKeys);
            let waits = parsed == Parsed::Incomplete
                && crate::vim::only_count_and_register(self.keys.so_far());
            if mouse && !y && !waits {
                self.picked = None;
                redraw = true;
            }
            match parsed {
                Parsed::Incomplete => redraw = true,
                // Keys a chat doesn't take, as `yic` (a chat has no
                // classes) or `dw`: Vim beeps, which stops a macro. Not
                // Esc, which only forgets what was typed, as Vim's does.
                Parsed::Nothing => {
                    redraw |= was_pending;
                    self.failed |= stroke != Stroke::Escape;
                }
                Parsed::Command(command) => {
                    let (drawn, insert) = self.execute(command);
                    if insert {
                        return (true, true);
                    }
                    redraw |= drawn;
                }
            }
        }
        (redraw, false)
    }

    /// Whether a yank or search failed since this was last asked, as Vim
    /// beeps: a macro or a binding typing keys stops.
    pub(crate) fn take_failed(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }

    /// Drops the yanks and searches normal or visual mode queued, as keys
    /// waiting for them are given up on.
    pub(crate) fn drop_reading(&mut self) {
        self.queued.retain(|input| {
            !matches!(
                input,
                Input::Yank { .. } | Input::YankText(_) | Input::Search { .. }
            )
        });
    }

    /// Whether a yank or a search normal or visual mode queued waits for
    /// the fonts.
    pub(crate) fn reading_unsettled(&self) -> bool {
        (self.queued.iter()).any(|input| {
            matches!(
                input,
                Input::Yank { .. } | Input::YankText(_) | Input::Search { .. }
            )
        })
    }

    /// Carries out `input`, a yank or a search normal or visual mode
    /// queued, now that the fonts are at hand to lay out what it needs.
    pub(super) fn settle_reading(
        &mut self,
        fonts: &mut crate::fonts::Fonts,
        input: Input,
        settled: &mut super::Settled,
    ) {
        match input {
            Input::Yank {
                start,
                end,
                lines,
                to,
            } => {
                let text = self.text_between(fonts, start, end);
                self.yanked(text, lines, to, settled);
            }
            Input::YankText(crate::pane::Yanked { register, to }) => {
                self.yanked(register.text, register.lines, to, settled);
            }
            Input::Search {
                from,
                backward,
                count,
            } => self.searched(fonts, from, backward, count),
            _ => {}
        }
    }

    /// Does `command`, read whole. Returns whether the chat must be
    /// redrawn, and whether it goes to insert mode, to type a message.
    fn execute(&mut self, command: Command<ChatKey>) -> (bool, bool) {
        match command.action {
            Action::Move(motion) => {
                let motion = self.finds.remember(motion);
                self.move_key(motion, command.count);
            }
            Action::FindAgain { reverse } => match self.finds.again(reverse) {
                Some(motion) => self.move_key(motion, command.count),
                None => return (false, false),
            },
            Action::Scroll(scroll) => {
                let moved = self.scroll_keeping_cursor(|chat| chat.scrolled(scroll, command.count));
                return (moved, false);
            }
            Action::Visual { lines } => {
                let kind = if lines {
                    Selection::Lines
                } else {
                    Selection::Characters
                };
                self.visual_key(self.visual(), kind);
            }
            Action::OtherEnd => {
                if let Some(picked) = &mut self.picked {
                    std::mem::swap(&mut picked.anchor, &mut picked.head);
                    self.cursor = Some(picked.head);
                    self.goal = None;
                    self.reveal = true;
                }
            }
            Action::Insert(_) | Action::Own(ChatKey::Compose) => {
                self.leave_visual();
                return (true, true);
            }
            Action::Operate(Operator::Yank, target) => {
                self.yank(target, command.count, command.register);
            }
            Action::Search { backward } => self.start_search(backward, command.count),
            Action::SearchAgain { reverse } => self.search_again(reverse, command.count),
            Action::Own(own) => return self.own(own),
            // Nothing else is taken (see `ChatKeys::takes`).
            _ => return (false, false),
        }
        self.follow_selection();
        (true, false)
    }

    /// Does `key`, a command of a chat's own.
    fn own(&mut self, key: ChatKey) -> (bool, bool) {
        match key {
            ChatKey::Compose => return (true, true),
            ChatKey::Enter => {
                if !self.fold_tool(None) {
                    let Some(cursor) = self.cursor.filter(|cursor| control(cursor.text.piece))
                    else {
                        self.leave_visual();
                        return (true, true);
                    };
                    let TextKey { turn, piece, part } = cursor.text;
                    match self.agent_on(turn, part).filter(|_| piece == AGENTS) {
                        Some(chat) => self.queued.push(Input::Act(ChatAction::Open { chat })),
                        None if piece == AGENTS => self.toggle_agents(turn),
                        None => self.toggle_fold(turn),
                    }
                }
            }
            // Esc leaves visual mode, keeping the last search's matches
            // lit, as Vim's does; or else stops a mouse's selection and
            // lighting them up, as `:nohlsearch` does.
            ChatKey::Escape => {
                if self.visual().is_none() {
                    self.stop_highlighting();
                }
                self.picked = None;
            }
            ChatKey::Detail => {
                if !self.toggle_detail() {
                    self.say("the agent hasn't said how much context the chat fills".to_owned());
                }
            }
            ChatKey::Undo { redo } => self.undo_key(redo),
            ChatKey::Resend => {
                if !self.resend() {
                    self.say(
                        "the last message didn't fail, so there's nothing to send again".into(),
                    );
                    return (false, false);
                }
            }
            ChatKey::OpenLink => {
                let found = (self.cursor).and_then(|cursor| self.link_at_spot(cursor));
                let Some(found) = found else {
                    self.say("no file or link under the cursor".to_owned());
                    return (false, false);
                };
                self.opening = Some(found.target);
            }
            ChatKey::Fold(fold) => return (self.fold_key(fold), false),
            ChatKey::Sideways { right, half } => {
                let pixels = if half {
                    self.cache.width / 2.0
                } else {
                    4.0 * self.cache.size
                };
                let moved = self.scroll_sideways(None, if right { pixels } else { -pixels });
                return (moved, false);
            }
        }
        self.follow_selection();
        (true, false)
    }

    /// Visual mode's selection follows the cursor.
    pub(super) fn follow_selection(&mut self) {
        if let (Some(picked), Some(cursor)) = (&mut self.picked, self.cursor)
            && matches!(picked.how, How::Visual(_))
        {
            picked.head = cursor;
        }
    }

    /// `motion` typed, `count` times: the cursor moves (see
    /// [`moved`](Self::moved)), or fails to, as `j` on the last line or
    /// `fx` with no x does, which Vim beeps at, stopping a macro.
    fn move_key(&mut self, motion: Motion, count: Option<usize>) {
        if !self.moved(motion, count) {
            self.failed = true;
        }
    }

    /// Moves the cursor by `motion`, `times` times, or until it goes no
    /// further, so a count as big as there is costs no more than the chat.
    /// Returns whether it went as Vim's would, without failing: a find
    /// found its character; a motion that can fail moved; `2$` found a line
    /// below to go to the end of, or else went nowhere, as Vim's
    /// `nv_dollar()`. Motions to a place, as `0` and `gg`, never fail (see
    /// [`Motion::can_fail`]).
    ///
    /// `b` and `B` with a count fail as Vim's `bck_word()` does: when one
    /// of the count starts at the chat's start, though the cursor got there.
    /// From anywhere else one always moves, so that's one that didn't,
    /// which is all the editor's `objects::back_fails` works out, from a
    /// text's lines, the chat not being one.
    pub(super) fn moved(&mut self, motion: Motion, count: Option<usize>) -> bool {
        let times = count.unwrap_or(1);
        if let Motion::Find {
            ch,
            forward,
            till,
            again,
        } = motion
        {
            return self.find((ch, forward, till, again), times);
        }
        if let (Motion::First | Motion::Last, Some(number)) = (motion, count) {
            self.go_to_turn_numbered(number);
            return true;
        }
        let from = self.cursor_or_default();
        // `2$` is the end of the next line, as many on as there are, as
        // Vim's: with none below, it fails, going nowhere.
        if motion == Motion::LineEnd && times > 1 {
            let goal = self.goal;
            for _ in 1..times {
                let before = self.cursor;
                self.vertical(Way::On);
                if self.cursor == before {
                    break;
                }
            }
            if self.cursor == from {
                self.goal = goal;
                return false;
            }
            self.line_edge('$');
            return true;
        }
        let mut stuck = false;
        for _ in 0..times {
            let before = (self.cursor, self.goal);
            self.move_once(motion);
            stuck |= self.cursor == before.0;
            if (self.cursor, self.goal) == before {
                break;
            }
            // `gg`, `G`, `0`, `^` and `$` go where they go, once.
            if matches!(
                motion,
                Motion::First
                    | Motion::Last
                    | Motion::LineStart
                    | Motion::FirstNonBlank
                    | Motion::LineEnd
            ) {
                break;
            }
        }
        let ran_out = stuck && matches!(motion, Motion::WordBack | Motion::BigWordBack);
        !motion.can_fail() || (self.cursor != from && !ran_out)
    }

    /// Moves the cursor once by `motion`, one of those [`ChatKeys`]
    /// takes.
    pub(super) fn move_once(&mut self, motion: Motion) {
        match motion {
            Motion::Down(_) | Motion::RowDown(_) => self.vertical(Way::On),
            Motion::Up(_) | Motion::RowUp(_) => self.vertical(Way::Back),
            Motion::Left | Motion::LeftOver => self.across(Way::Back),
            Motion::Right | Motion::RightOver => self.across(Way::On),
            Motion::WordStart => self.word(Way::On, false, false),
            Motion::WordEnd => self.word(Way::On, true, false),
            Motion::WordBack => self.word(Way::Back, false, false),
            Motion::BigWordStart => self.word(Way::On, false, true),
            Motion::BigWordEnd => self.word(Way::On, true, true),
            Motion::BigWordBack => self.word(Way::Back, false, true),
            Motion::LineStart => self.line_edge('0'),
            Motion::FirstNonBlank => self.line_edge('^'),
            Motion::LineEnd => self.line_edge('$'),
            // On the last line, or the first, they go nowhere, failing, as
            // Vim's do.
            Motion::NextLine | Motion::PreviousLine => {
                let before = self.cursor;
                let way = if motion == Motion::NextLine {
                    Way::On
                } else {
                    Way::Back
                };
                self.vertical(way);
                if self.cursor != before {
                    self.line_edge('^');
                }
            }
            Motion::Paragraph { forward } => {
                self.block(if forward { Way::On } else { Way::Back });
            }
            Motion::First => self.go_to_end(Way::Back),
            Motion::Last => self.go_to_end(Way::On),
            _ => {}
        }
    }

    /// Takes the view's height, `height` pixels: one of another number of
    /// lines forgets the half page a count set, as Vim's `scroll` is set
    /// afresh when the window's height changes.
    pub(super) fn set_view_height(&mut self, height: f32) {
        let rows = |height: f32| (height / self.line_height.max(1.0)).floor();
        if rows(height).to_bits() != rows(self.view_height).to_bits() {
            self.paging.resized();
        }
        self.view_height = height;
    }

    /// Ctrl+E, Ctrl+Y, Ctrl+D, Ctrl+U, Ctrl+F and the pages, `count`
    /// typed, as far as they go in any pane (see [`crate::vim::Paging`]).
    /// Returns whether the view moved.
    fn scrolled(&mut self, scroll: Scroll, count: Option<usize>) -> bool {
        let (Scroll::Line { down } | Scroll::HalfPage { down } | Scroll::Page { down }) = scroll;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rows in view"
        )]
        let view = (self.view_height / self.line_height.max(1.0))
            .floor()
            .max(1.0) as u64;
        #[allow(clippy::cast_precision_loss, reason = "rows in view")]
        let rows = self.paging.rows(scroll, count, view) as f32;
        self.scroll_by(if down { rows } else { -rows } * self.line_height)
    }

    /// `u`, or with `redo` Ctrl+R: undoes what the turn the cursor's in
    /// changed in files, or makes it again, as Vim's undo and redo do, if
    /// that turn's in view and has something to undo or redo; or else the
    /// last turn that has, or the first to redo. The bar says which, or
    /// that none has.
    fn undo_key(&mut self, redo: bool) {
        // The cursor's turn only if it's in view: what's undone is never
        // out of sight.
        let here = (self.cursor.map(|cursor| cursor.text.turn)).filter(|&turn| {
            (self.shown.iter()).any(|shown| shown.key.turn == turn)
                && (self.node.turns.get(turn)).is_some_and(|turn| super::undoable(turn, redo))
        });
        self.undo_turn(here.or_else(|| self.turn_to_undo(redo)), redo);
    }

    /// Undoes what `turn` changed in files, or with `redo` makes it again,
    /// saying so in the bar, or that there's no such turn if `None`.
    pub(super) fn undo_turn(&mut self, turn: Option<usize>, redo: bool) {
        let Some(turn) = turn else {
            let undo = if redo { "redo" } else { "undo" };
            self.say(format!("no turn has files to {undo}"));
            return;
        };
        // It rewrites files: the bar says which turn's.
        let files = match self.node.turns[turn].files.map_or(0, |files| files.count) {
            1 => "1 file".to_owned(),
            count => format!("{count} files"),
        };
        let doing = if redo { "Redoing" } else { "Undoing" };
        let number = clackatoo_protocol::turn_number(&self.node.turns, turn).unwrap_or(turn + 1)
            + self.node.first_message as usize;
        self.say(format!("{doing} turn {number} ({files})"));
        let turn = u32::try_from(turn).unwrap_or(u32::MAX);
        self.queued.push(Input::Act(if redo {
            ChatAction::Redo { turn, force: false }
        } else {
            ChatAction::Undo { turn, force: false }
        }));
    }

    /// `za`, `zo`, `zc`, `zR` or `zM`: opens or folds the work of the turn
    /// the cursor's in, or of every turn; on a tool, the tool; on the card
    /// of the chats a turn started, the card. Returns whether it did.
    fn fold_key(&mut self, fold: Fold) -> bool {
        let open = match fold {
            Fold::Toggle => None,
            Fold::Open => Some(true),
            Fold::Close => Some(false),
            Fold::All { open } => {
                self.set_all_folds(open);
                self.set_all_agents(open);
                self.after_fold();
                return true;
            }
        };
        if self.fold_tool(open) {
            return true;
        }
        let Some(turn) = self.cursor.map(|cursor| cursor.text.turn) else {
            return false;
        };
        // On the card of the chats the turn started, it's the card that
        // opens and folds.
        let card = self
            .cursor
            .is_some_and(|cursor| cursor.text.piece == AGENTS);
        match (open, card) {
            (None, true) => self.toggle_agents(turn),
            (Some(open), true) => self.set_agents(turn, open),
            (None, false) => self.toggle_fold(turn),
            (Some(open), false) => self.set_fold(turn, open),
        }
        self.after_fold();
        true
    }

    /// Puts the cursor where it can be after a fold: off a chat's line on
    /// a card folded away, onto the card's own, and out of a turn's work
    /// folded away, onto the line it folds under.
    fn after_fold(&mut self) {
        if let Some(cursor) = self.cursor
            && cursor.text.piece == AGENTS
            && cursor.text.part > 0
            && !self.agents_open(cursor.text.turn)
        {
            self.cursor = Some(Position {
                text: TextKey {
                    part: 0,
                    ..cursor.text
                },
                byte: 0,
            });
            self.picked = None;
        }
        if let Some(cursor) = self.cursor
            && cursor.text.piece >= ITEMS
            && let Some(turn) = self.node.turns.get(cursor.text.turn)
        {
            let shape = self.shape(cursor.text.turn, turn);
            if !shape.open && cursor.text.piece - ITEMS < shape.answer {
                self.cursor = Some(Position {
                    text: TextKey {
                        piece: FOLD,
                        part: 0,
                        ..cursor.text
                    },
                    byte: 0,
                });
                self.picked = None;
            }
        }
        self.cursor_off_folded_work();
        self.reveal = true;
    }

    /// Opens the cards the tool the agent asks about is folded under, as a
    /// subagent's: what's asked about always shows.
    pub(super) fn open_cards_over_asked(&mut self) {
        let Some(id) = (self.node.question.as_ref()).and_then(|question| question.tool.clone())
        else {
            return;
        };
        for index in (0..self.node.turns.len()).rev() {
            let turn = &self.node.turns[index];
            let item =
                (turn.items.iter()).find(|item| item.tool().is_some_and(|tool| tool.id == id));
            if let Some(item) = item {
                let folded =
                    super::paint::nested::Nesting::of(turn).folded_above(item, &self.open_cards);
                if !folded.is_empty() {
                    self.open_cards.extend(folded);
                    self.restyled(index);
                }
                return;
            }
        }
    }

    /// Puts the cursor, if it's on what a subagent did that's folded away
    /// under its tool's card, onto the card that shows in its place.
    pub(super) fn cursor_off_folded_work(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let Some(turn) = self.node.turns.get(cursor.text.turn) else {
            return;
        };
        let item = (cursor.text.piece.checked_sub(ITEMS)).and_then(|item| turn.items.get(item));
        let card = item.and_then(|item| {
            super::paint::nested::Nesting::of(turn).shown_above(item, &self.open_cards)
        });
        if let Some(card) = card {
            self.cursor = Some(Position {
                text: TextKey {
                    piece: ITEMS + card,
                    part: 0,
                    ..cursor.text
                },
                byte: 0,
            });
            self.picked = None;
        }
    }

    /// Opens the tool under the reading cursor, keeping the cursor on its
    /// header when its output disappears. Returns false outside a tool.
    fn fold_tool(&mut self, open: Option<bool>) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let Some(item) = cursor.text.piece.checked_sub(ITEMS) else {
            return false;
        };
        let Some(turn) = self.node.turns.get(cursor.text.turn) else {
            return false;
        };
        // In a subagent's words, it's the card of the tool that started
        // it that opens and folds, as Vim's keys act on the fold around.
        let card = match turn.items.get(item) {
            Some(Item::Nested {
                parent,
                item: inner,
            }) if inner.tool().is_none() => (turn.items)
                .iter()
                .position(|item| item.tool().is_some_and(|tool| tool.id == *parent)),
            _ => Some(item),
        };
        let Some((card, tool)) = card.and_then(|card| Some((card, turn.items.get(card)?.tool()?)))
        else {
            return false;
        };
        let id = tool.id.clone();
        let open = open.unwrap_or_else(|| !self.open_cards.contains(&id));
        if open {
            self.open_cards.insert(id);
        } else {
            self.open_cards.remove(&id);
        }
        self.restyled(cursor.text.turn);
        self.cursor = Some(Position {
            text: TextKey {
                piece: ITEMS + card,
                part: 0,
                ..cursor.text
            },
            byte: 0,
        });
        self.picked = None;
        self.reveal = true;
        true
    }

    /// `v` or `V`: starts selecting `kind` from the cursor, or changes to
    /// it, or if visual mode selects it already, stops.
    fn visual_key(&mut self, visual: Option<Selection>, kind: Selection) {
        if visual == Some(kind) {
            self.picked = None;
            return;
        }
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let anchor = (self.picked)
            .filter(|_| visual.is_some())
            .map_or(cursor, |picked| picked.anchor);
        self.picked = Some(Picked {
            anchor,
            head: cursor,
            how: How::Visual(kind),
        });
    }

    /// Leaves visual mode, if it's in it, selecting nothing.
    fn leave_visual(&mut self) {
        if self.visual().is_some() {
            self.picked = None;
        }
    }

    /// The whole of the piece text `key` is in, as it's copied: a message
    /// as it was typed, the agent's words as the Markdown they came as, a
    /// command and its output, a change as a diff, or for the line a
    /// turn's work folds under, the turn.
    pub(super) fn piece_text(&self, key: TextKey) -> Option<String> {
        let turn = self.node.turns.get(key.turn)?;
        match key.piece {
            PROMPT => Some(turn.prompt.clone()),
            AGENTS => Some(self.agents_text(key.turn)),
            FOLD => {
                let alone = ChatNode {
                    turns: vec![turn.clone()],
                    ..ChatNode::default()
                };
                Some(alone.text().trim_end().to_owned())
            }
            piece => match turn.items.get(piece - ITEMS)?.inner() {
                Item::Words(text) | Item::Thought(text) | Item::Other(text) => Some(text.clone()),
                Item::Compacted(details) => Some(Item::compacted_label(details)),
                Item::Tool(tool) => {
                    Some(Card::of(tool).map_or_else(|| tool.title.clone(), |card| card.text()))
                }
                Item::Nested { .. } => None,
            },
        }
    }

    /// The cursor, or if there's none yet, one at the start of the last
    /// line in view.
    pub(super) fn cursor_or_default(&mut self) -> Option<Position> {
        if self.cursor.is_none() {
            self.cursor = self.default_cursor();
        }
        self.cursor
    }

    /// Where the cursor starts: the start of the last line in view, even
    /// of a block that runs on below it.
    fn default_cursor(&self) -> Option<Position> {
        for shown in self.shown.iter().rev() {
            if let Some(line) = self.last_line_in_view(shown) {
                let text = self.text(shown.key)?;
                let byte = (super::paint::line_middle(text.layout, line))
                    .map_or(0, |(_, range)| range.start);
                return Some(Position {
                    text: shown.key,
                    byte,
                });
            }
        }
        let first = self.shown.first()?;
        Some(Position {
            text: first.key,
            byte: 0,
        })
    }

    /// The last line of `shown` whose middle is in view, if any is: none
    /// of a table's cell scrolled out of it sideways.
    fn last_line_in_view(&self, shown: &super::paint::Shown) -> Option<usize> {
        if !self.across_in_view(shown) {
            return None;
        }
        let text = self.text(shown.key)?;
        let in_view = |middle: f32| (0.0..self.view_height).contains(&(shown.at[1] + middle));
        (0..text.layout.len()).rev().find(|&line| {
            super::paint::line_middle(text.layout, line).is_some_and(|(middle, _)| in_view(middle))
        })
    }

    /// Whether some of `shown` is in the column, across: a table's cell
    /// scrolled sideways may not be.
    fn across_in_view(&self, shown: &super::paint::Shown) -> bool {
        let left = self.column_right - self.cache.width;
        shown.at[0] < self.column_right && shown.at[0] + shown.size[0].max(1.0) > left
    }

    /// Coming into normal mode, puts the cursor where it shows: on the
    /// last line in view, unless it's in view already. So pressing Ctrl+;
    /// always shows a cursor, never one left above the view, or none.
    pub(super) fn cursor_on_arrival(&mut self) {
        let arrived = self.reading && !self.was_reading;
        self.was_reading = self.reading;
        if !arrived || self.picked.is_some() {
            return;
        }
        let shown = self.cursor.is_some_and(|cursor| {
            let Some(shown) = self.shown.iter().find(|shown| shown.key == cursor.text) else {
                return false;
            };
            if !self.across_in_view(shown) {
                return false;
            }
            let Some(text) = self.text(cursor.text) else {
                return false;
            };
            let line = super::paint::line_of(text.layout, cursor.byte);
            (super::paint::line_middle(text.layout, line)).is_some_and(|(middle, _)| {
                (0.0..self.view_height).contains(&(shown.at[1] + middle))
            })
        });
        if !shown {
            self.cursor = self.default_cursor();
            self.goal = None;
        }
    }

    /// Puts the cursor at `position`, and scrolls to it.
    pub(super) fn go(&mut self, position: Position) {
        self.cursor = Some(position);
        self.reveal = true;
    }

    /// The block of text after `key`, or before, as the conversation shows
    /// them. A turn not laid out gives the start of it, which is laid out
    /// as the view goes to it.
    pub(super) fn next_text(&self, key: TextKey, way: Way) -> Option<TextKey> {
        let mut turn = key.turn;
        loop {
            let texts = self.turn_texts(turn);
            let found = match way {
                Way::On => texts.into_iter().find(|text| *text > key),
                Way::Back => texts.into_iter().rev().find(|text| *text < key),
            };
            if found.is_some() {
                return found;
            }
            turn = match way {
                Way::On => Some(turn + 1).filter(|&next| next < self.node.turns.len())?,
                Way::Back => turn.checked_sub(1)?,
            };
            if (self.cache.turns.get(turn)).is_none_or(|slot| !slot.laid_out()) {
                return Some(TextKey {
                    turn,
                    piece: PROMPT,
                    part: 0,
                });
            }
        }
    }

    /// `j` or `k`: a line down or up, as near the same distance across as
    /// the text allows.
    fn vertical(&mut self, way: Way) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            if let Some(key) = self.next_text(cursor.text, way) {
                self.go(Position { text: key, byte: 0 });
            }
            return;
        };
        let goal = self
            .goal
            .unwrap_or_else(|| text.at[0] + super::paint::x_of(text.layout, cursor.byte));
        let line = super::paint::line_of(text.layout, cursor.byte);
        let within = match way {
            Way::On => Some(line + 1).filter(|&next| next < text.layout.len()),
            Way::Back => line.checked_sub(1),
        };
        let (key, line) = if let Some(line) = within {
            (cursor.text, Some(line))
        } else {
            // In a table, up and down go to the cell above or below, and
            // off its top or bottom, to what's before or after it.
            let next = match self.table_step(cursor.text, way == Way::Back) {
                Some(Ok(cell)) => Some(cell),
                Some(Err(edge)) => self.next_text(edge, way),
                None => self.next_text(cursor.text, way),
            };
            let Some(key) = next else {
                return;
            };
            let line = self.text(key).map(|text| match way {
                Way::On => 0,
                Way::Back => text.layout.len().saturating_sub(1),
            });
            (key, line)
        };
        // On a character, as in Vim: not past the line's last.
        let byte = match (self.text(key), line) {
            (Some(text), Some(line)) => {
                super::paint::line_middle(text.layout, line).map_or(0, |(middle, range)| {
                    let x = goal - text.at[0];
                    let byte = parley::Cursor::from_point(text.layout, x, middle).index();
                    byte.min(last_char(text.text, range))
                })
            }
            _ => 0,
        };
        self.go(Position { text: key, byte });
        self.goal = Some(goal);
    }

    /// `h` or `l`: a character back or on, within the line.
    fn across(&mut self, way: Way) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let line = super::paint::line_of(text.layout, cursor.byte);
        let Some((_, range)) = super::paint::line_middle(text.layout, line) else {
            return;
        };
        let byte = super::paint::within(text.text, cursor.byte);
        let byte = match way {
            // By what's drawn as one character, as an emoji of several.
            Way::Back if byte > range.start => {
                super::paint::before(text.text, byte).max(range.start)
            }
            Way::On if byte < last_char(text.text, range.clone()) => {
                super::paint::after(text.text, byte)
            }
            _ => byte,
        };
        self.go(Position { byte, ..cursor });
        self.goal = None;
    }

    /// `0`, `^` or `$`: the start of the line, its first character that
    /// isn't blank, or its last character.
    fn line_edge(&mut self, edge: char) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let line = super::paint::line_of(text.layout, cursor.byte);
        let Some((_, range)) = super::paint::line_middle(text.layout, line) else {
            return;
        };
        let byte = match edge {
            '0' => range.start,
            '^' => {
                let line = &text.text[range.start..range.end.min(text.text.len())];
                range.start + line.len() - line.trim_start().len()
            }
            _ => last_char(text.text, range),
        };
        self.go(Position { byte, ..cursor });
        self.goal = None;
    }

    /// `w`, `e` or `b`: the start of the next word, the end of this one or
    /// the next, or the start of this one or the one before, going on to
    /// the next block, or back to the one before, at either end; `W`, `E`
    /// and `B` if `big`, for anything between blanks.
    fn word(&mut self, way: Way, end: bool, big: bool) {
        let kind = if big { big_class } else { class };
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let byte = super::paint::within(text.text, cursor.byte);
        let found = match (way, end) {
            (Way::On, false) => next_word_start(text.text, byte, kind),
            (Way::On, true) => word_end(text.text, byte, kind),
            (Way::Back, _) => word_start_before(text.text, byte, kind),
        };
        if let Some(byte) = found {
            self.go(Position { byte, ..cursor });
        } else if let Some(key) = self.next_text(cursor.text, way) {
            let byte = match (way, self.text(key)) {
                (Way::Back, Some(text)) => {
                    word_start_before(text.text, text.text.len(), kind).unwrap_or(0)
                }
                (Way::On, Some(text)) if end => word_end(text.text, 0, kind).unwrap_or(0),
                _ => 0,
            };
            self.go(Position { text: key, byte });
        }
        self.goal = None;
    }

    /// `f`, `t`, `F` or `T` (`forward`, `till`) and `ch`, `count` times:
    /// the `count`th `ch` on the cursor's line after it, or before it, or
    /// the character just before that, or after it going back; found
    /// `again`, by `;` or `,`, a `t` or `T` doesn't stop at one right by
    /// the cursor, as Vim's doesn't. Nowhere if there aren't so many.
    /// Returns whether it found one.
    fn find(&mut self, find: (char, bool, bool, bool), count: usize) -> bool {
        let Some(cursor) = self.cursor_or_default() else {
            return false;
        };
        let Some(text) = self.text(cursor.text) else {
            return false;
        };
        let line = super::paint::line_of(text.layout, cursor.byte);
        let Some((_, range)) = super::paint::line_middle(text.layout, line) else {
            return false;
        };
        let found = find_in(text.text, range, cursor.byte, find, count);
        if let Some(byte) = found {
            self.go(Position { byte, ..cursor });
        }
        self.goal = None;
        found.is_some()
    }

    /// `{` or `}`: the start of the block before, or after.
    fn block(&mut self, way: Way) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let key = match way {
            Way::Back if cursor.byte > 0 => Some(cursor.text),
            _ => self.next_text(cursor.text, way),
        };
        if let Some(key) = key {
            self.go(Position { text: key, byte: 0 });
        }
        self.goal = None;
    }

    /// `gg` or `G`: the start of the chat, or the start of its last line.
    fn go_to_end(&mut self, way: Way) {
        let Some(last) = self.node.turns.len().checked_sub(1) else {
            return;
        };
        self.go_to_turn(if way == Way::Back { 0 } else { last }, way);
    }

    /// `5gg` or `5G`: the start of the turn numbered so, as the chat
    /// numbers them, counting from 1, or the first loaded if it's before
    /// those loaded, or the last if there aren't so many: a chat's turns
    /// are what a file's lines are to `G`.
    fn go_to_turn_numbered(&mut self, number: usize) {
        let Some(last) = self.node.turns.len().checked_sub(1) else {
            return;
        };
        let first = self.node.first_message as usize;
        let turn = if number <= first {
            0
        } else {
            clackatoo_protocol::turn_numbered(&self.node.turns, number - first).unwrap_or(last)
        };
        self.go_to_turn(turn, Way::Back);
    }

    /// To the start of turn `turn`, or the start of its last line if
    /// `way` is on, as `gg` and `G` go to the chat's ends.
    fn go_to_turn(&mut self, turn: usize, way: Way) {
        let last = self.node.turns.len().saturating_sub(1);
        let texts = self.turn_texts(turn);
        let key = match way {
            Way::Back => texts.first(),
            Way::On => texts.last(),
        };
        let position = match key {
            Some(&key) => {
                let byte = match (way, self.text(key)) {
                    (Way::On, Some(text)) => {
                        let line = text.layout.len().saturating_sub(1);
                        super::paint::line_middle(text.layout, line)
                            .map_or(0, |(_, range)| range.start)
                    }
                    _ => 0,
                };
                Position { text: key, byte }
            }
            // Not laid out: its start, or past its end, until it is.
            None if way == Way::Back => Position {
                text: TextKey {
                    turn,
                    piece: PROMPT,
                    part: 0,
                },
                byte: 0,
            },
            None => Position {
                text: TextKey {
                    turn,
                    piece: usize::MAX,
                    part: 0,
                },
                byte: usize::MAX,
            },
        };
        self.go(position);
        self.goal = None;
        if way == Way::On && turn == last {
            self.follow = true;
        }
    }

    /// Scrolls as `scroll` does, and if the cursor goes out of view, it
    /// comes back into it, at the edge it went past, once the chat's
    /// drawn. Returns whether the view moved.
    fn scroll_keeping_cursor(&mut self, scroll: impl FnOnce(&mut Self) -> bool) -> bool {
        let moved = scroll(self);
        self.keep_cursor = moved;
        moved
    }

    /// Puts the cursor on text its turn shows, now it's laid out, if it
    /// was put at the start or the end of the turn before it was: on the
    /// first text, or the start of the last line of the last.
    pub(super) fn cursor_onto_text(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if self.text(cursor.text).is_some() {
            return;
        }
        let texts = self.turn_texts(cursor.text.turn);
        let back = texts.iter().rev().find(|&&key| key <= cursor.text).copied();
        let Some(key) = back.or_else(|| texts.first().copied()) else {
            return;
        };
        let byte = match (back, self.text(key)) {
            (Some(_), Some(text)) => {
                let line = text.layout.len().saturating_sub(1);
                super::paint::line_middle(text.layout, line).map_or(0, |(_, range)| range.start)
            }
            _ => 0,
        };
        let position = Position { text: key, byte };
        self.cursor = Some(position);
        if let Some(picked) = &mut self.picked
            && matches!(picked.how, How::Visual(_))
            && picked.head == cursor
        {
            picked.head = position;
        }
    }

    /// Brings the cursor back into view, if scrolling took it out: to the
    /// first line in view, or the last.
    pub(super) fn cursor_into_view(&mut self) {
        if !std::mem::take(&mut self.keep_cursor) {
            return;
        }
        let Some(cursor) = self.cursor else {
            return;
        };
        let height = self.view_height;
        let in_view = |shown: &&super::paint::Shown| {
            !control(shown.key.piece) && shown.at[1] >= 0.0 && shown.at[1] + shown.size[1] <= height
        };
        if self
            .shown
            .iter()
            .filter(in_view)
            .any(|shown| shown.key == cursor.text)
        {
            return;
        }
        let above = (self.shown.first()).is_some_and(|first| cursor.text < first.key);
        let shown = if above {
            self.shown.iter().find(in_view)
        } else {
            self.shown.iter().rev().find(in_view)
        };
        if let Some(shown) = shown {
            let key = shown.key;
            let byte = match (above, self.text(key)) {
                (false, Some(text)) => {
                    let line = text.layout.len().saturating_sub(1);
                    super::paint::line_middle(text.layout, line).map_or(0, |(_, range)| range.start)
                }
                _ => 0,
            };
            self.cursor = Some(Position { text: key, byte });
            if let Some(picked) = &mut self.picked
                && matches!(picked.how, How::Visual(_))
            {
                picked.head = Position { text: key, byte };
            }
        }
    }
}

/// Where `f`, `t`, `F` or `T` (`forward`, `till`) and `ch`, `count`
/// times, go from `byte` in `text`, within the line `range` holds, as
/// [`ChatView::find`] says; `None` if there aren't so many.
pub(super) fn find_in(
    text: &str,
    range: std::ops::Range<usize>,
    byte: usize,
    (ch, forward, till, again): (char, bool, bool, bool),
    count: usize,
) -> Option<usize> {
    let end = range.end.min(text.len());
    let start = range.start.min(end);
    let chars: Vec<(usize, char)> = text[start..end]
        .trim_end_matches(['\n', '\r'])
        .char_indices()
        .map(|(at, ch)| (start + at, ch))
        .collect();
    let byte = super::paint::within(text, byte);
    let here = chars.iter().position(|&(at, _)| at >= byte)?;
    let skip = usize::from(till && again);
    let found = if forward {
        (chars.iter().enumerate())
            .skip(here + 1 + skip)
            .filter(|(_, (_, found))| *found == ch)
            .nth(count.max(1) - 1)
            .map(|(index, _)| if till { index - 1 } else { index })
    } else {
        (chars[..here.saturating_sub(skip)].iter().enumerate())
            .rev()
            .filter(|(_, (_, found))| *found == ch)
            .nth(count.max(1) - 1)
            .map(|(index, _)| if till { index + 1 } else { index })
    };
    found.map(|index| chars[index].0)
}

/// The start of the last character of the line of `text` in `range`,
/// before its line break.
fn last_char(text: &str, range: std::ops::Range<usize>) -> usize {
    let end = range.end.min(text.len());
    let line = text[range.start.min(end)..end].trim_end_matches(['\n', '\r']);
    line.char_indices()
        .next_back()
        .map_or(range.start, |(at, _)| range.start + at)
}

/// What sort of character `ch` is, for words: blank, a word's, or other
/// punctuation, as Vim's `w` sees them.
fn class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_alphanumeric() || ch == '_' {
        1
    } else {
        2
    }
}

/// What sort of character `ch` is, for WORDs: blank, or not, as Vim's `W`
/// sees them.
fn big_class(ch: char) -> u8 {
    u8::from(!ch.is_whitespace())
}

/// Where the next word after the one at `byte` in `text` starts, if one
/// does, words as `class` sorts characters.
fn next_word_start(text: &str, byte: usize, class: fn(char) -> u8) -> Option<usize> {
    let mut chars = text[byte..].char_indices().map(|(at, ch)| (byte + at, ch));
    let (_, first) = chars.next()?;
    let mut was = class(first);
    for (at, ch) in chars {
        let is = class(ch);
        if is != 0 && is != was {
            return Some(at);
        }
        was = is;
    }
    None
}

/// Where the word at `byte` in `text` ends, or if `byte` is at its end
/// already, the next one: its last character.
fn word_end(text: &str, byte: usize, class: fn(char) -> u8) -> Option<usize> {
    let chars: Vec<(usize, char)> = text[byte..]
        .char_indices()
        .map(|(at, ch)| (byte + at, ch))
        .collect();
    let mut index = 1;
    while index < chars.len() && class(chars[index].1) == 0 {
        index += 1;
    }
    let class_here = class(chars.get(index)?.1);
    while index + 1 < chars.len() && class(chars[index + 1].1) == class_here {
        index += 1;
    }
    Some(chars[index].0)
}

/// Where the word before `byte` in `text` starts, or the one `byte` is
/// in, if it's not at its start.
fn word_start_before(text: &str, byte: usize, class: fn(char) -> u8) -> Option<usize> {
    let chars: Vec<(usize, char)> = text[..byte].char_indices().collect();
    let mut index = chars.len().checked_sub(1)?;
    while class(chars[index].1) == 0 {
        index = index.checked_sub(1)?;
    }
    let class_here = class(chars[index].1);
    while index > 0 && class(chars[index - 1].1) == class_here {
        index -= 1;
    }
    Some(chars[index].0)
}

#[cfg(test)]
mod tests {
    use winit::keyboard::NamedKey;

    use super::{big_class, class, next_word_start, word_end, word_start_before};
    use crate::chat::ChatView;
    use crate::chat::fixtures::{chars, fonts, lines, paint, read, read_named, talk, yanked};
    use crate::chat::paint::{FOLD, ITEMS, PROMPT, TextKey};
    use crate::vim::Selection;

    impl ChatView {
        /// Where normal mode's cursor is in its text, by byte, for tests
        /// outside the chat.
        pub(crate) fn cursor_byte(&self) -> Option<usize> {
            self.cursor.map(|cursor| cursor.byte)
        }
    }

    fn reading(node: clackatoo_protocol::ChatNode) -> (ChatView, crate::fonts::Fonts) {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), node);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        (chat, fonts)
    }

    fn key(turn: usize, piece: usize) -> TextKey {
        TextKey {
            turn,
            piece,
            part: 0,
        }
    }

    #[test]
    fn the_cursor_starts_at_the_end_and_moves_through_the_text() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "first answer"), ("two", "second")]));
        // Nothing's moved it: it's on the last line in view.
        read(&mut chat, "l");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(1, ITEMS)));
        assert_eq!(chat.cursor.unwrap().byte, 1);
        read(&mut chat, "gg");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, PROMPT)));
        // `j` keeps to the same distance across, and the message is on
        // the right: `}` goes to the answer's start.
        read(&mut chat, "}w");
        let cursor = chat.cursor.unwrap();
        assert_eq!((cursor.text, cursor.byte), (key(0, ITEMS), 6));
        read(&mut chat, "$");
        assert_eq!(chat.cursor.unwrap().byte, "first answer".len() - 1);
        read(&mut chat, "jk0");
        assert_eq!(chat.cursor.unwrap().byte, 0);
        paint(&mut chat, &mut fonts);
        // `G` goes to the end.
        read(&mut chat, "G");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(1, ITEMS)));
    }

    #[test]
    fn a_motion_that_goes_nowhere_fails_unless_it_goes_to_a_place() {
        let (mut chat, _fonts) = reading(talk(&[("one", "first answer")]));
        // The first key, before there's a cursor, starts from where it
        // would be: on the last line, `j` goes nowhere.
        chat.cursor = None;
        read(&mut chat, "j");
        assert!(chat.take_failed());
        read(&mut chat, "G0");
        assert!(!chat.take_failed());
        // `j` on the last line, `h` at a line's start and a find that
        // finds nothing fail, as Vim's beep, which stops a macro.
        for keys in ["j", "h", "fZ", "3fe"] {
            read(&mut chat, keys);
            assert!(chat.take_failed(), "{keys}");
        }
        // `t` with its character right by the cursor stays, failing
        // nothing, as do `0`, `gg`, `G` where they are.
        read(&mut chat, "ti");
        assert!(!chat.take_failed());
        // Esc after a key waiting for more only forgets it, as Vim's.
        read(&mut chat, "y");
        read_named(&mut chat, NamedKey::Escape);
        assert!(!chat.take_failed());
        for keys in ["0", "G", "gg", "gg"] {
            read(&mut chat, keys);
            assert!(!chat.take_failed(), "{keys}");
        }
        // A count goes as far as it can, failing only if it goes nowhere.
        read(&mut chat, "G099l");
        assert!(!chat.take_failed());
        assert_eq!(chat.cursor.unwrap().byte, "first answer".len() - 1);
        // But as in Vim, `+` on the last line fails, though `^`
        // would move; `2$` there, with no line below to end, going
        // nowhere; and `b` that starts one of its count at the chat's
        // start.
        for keys in ["G$+", "G02$", "G0w3b"] {
            read(&mut chat, keys);
            assert!(chat.take_failed(), "{keys:?}");
        }
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, PROMPT)));
        read(&mut chat, "G02$");
        assert!(chat.take_failed());
        assert_eq!(chat.cursor.unwrap().byte, 0, "2$ went nowhere");
        // `2b` to the chat's start exactly fails nothing.
        read(&mut chat, "G0w2b");
        assert!(!chat.take_failed());
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, PROMPT)));
        read(&mut chat, "G0w$");
        assert_eq!(chat.cursor.unwrap().byte, "first answer".len() - 1);
        assert!(!chat.take_failed());
    }

    #[test]
    fn coming_into_normal_mode_shows_the_cursor_on_the_last_line_in_view() {
        use crate::render::{Ink, Paint};
        // A long chat, scrolled up a little, so its last block runs on
        // below the view.
        let mut node = crate::chat::fixtures::long_chat(3);
        let last = node.turns.len() - 1;
        let long = "A long answer, line after line of it. ".repeat(200);
        node.turns[last]
            .items
            .push(clackatoo_protocol::Item::Words(long));
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), node);
        paint(&mut chat, &mut fonts);
        chat.scroll_lines(10.0);
        paint(&mut chat, &mut fonts);
        assert!(chat.cursor.is_none());
        chat.reading = true;
        let drawn = paint(&mut chat, &mut fonts);
        let cursor = chat.cursor.expect("a cursor, at once");
        let shown = (chat.shown.iter())
            .find(|shown| shown.key == cursor.text)
            .expect("in view");
        let text = chat.text(cursor.text).unwrap();
        let line = crate::chat::paint::line_of(text.layout, cursor.byte);
        let (middle, _) = crate::chat::paint::line_middle(text.layout, line).unwrap();
        let y = shown.at[1] + middle;
        assert!(y >= 0.0 && y < chat.view_height, "{y}");
        // The last line in view: the one after is out of it.
        let (next, _) = crate::chat::paint::line_middle(text.layout, line + 1).unwrap();
        assert!(shown.at[1] + next >= chat.view_height);
        // Drawn solid, with the character under it drawn over it.
        assert!(drawn.over.iter().any(|fill| fill.paint == Paint::Accent));
        assert!(
            drawn
                .over_glyphs
                .iter()
                .any(|glyph| glyph.ink == Ink::OnColor)
        );
        // Moved to the top, it stays there while in normal mode...
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        let top = chat.cursor;
        paint(&mut chat, &mut fonts);
        assert_eq!(chat.cursor, top);
        // ...and, out of view after typing for a while, comes back into it
        // when normal mode does.
        chat.reading = false;
        chat.scroll = chat.max_scroll;
        paint(&mut chat, &mut fonts);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        assert_ne!(chat.cursor, top);
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(cursor.text));
    }

    #[test]
    fn visual_mode_copies_characters_or_lines_across_messages() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "first answer"), ("two", "second")]));
        read(&mut chat, "ggVj");
        assert_eq!(chat.visual(), Some(Selection::Lines));
        assert_eq!(read(&mut chat, "y"), (true, false));
        assert_eq!(chat.visual(), None);
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("one\n\nfirst answer\n")]
        );
        // Characters: the cursor's own included.
        read(&mut chat, "gg}wvly");
        assert_eq!(yanked(&mut chat, &mut fonts), [chars("an")]);
        // `Y` takes the lines.
        read(&mut chat, "gg}wvlY");
        assert_eq!(yanked(&mut chat, &mut fonts), [lines("first answer\n")]);
        // `o` goes to the other end, and Esc stops selecting.
        read(&mut chat, "vho");
        read_named(&mut chat, NamedKey::Escape);
        assert_eq!(chat.visual(), None);
    }

    #[test]
    fn enter_and_z_open_and_fold_the_turn_under_the_cursor() {
        let node = crate::chat::fixtures::long_chat(2);
        let (mut chat, mut fonts) = reading(node);
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        // The block after the message: the line its work folds under.
        read(&mut chat, "}");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, FOLD)));
        let open = |chat: &ChatView| chat.shape(0, &chat.node.turns[0]).open;
        assert!(!open(&chat));
        assert_eq!(read_named(&mut chat, NamedKey::Enter), (true, false));
        assert!(open(&chat));
        paint(&mut chat, &mut fonts);
        // Into the work, and `zc` folds it, the cursor going to its line.
        read(&mut chat, "}");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, ITEMS)));
        read(&mut chat, "zc");
        assert!(!open(&chat));
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, FOLD)));
        read(&mut chat, "zo");
        assert!(open(&chat));
        read(&mut chat, "zM");
        assert!(!open(&chat));
        read(&mut chat, "za");
        assert!(open(&chat));
        // Enter anywhere else types a message.
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg");
        assert_eq!(read_named(&mut chat, NamedKey::Enter), (true, true));
    }

    #[test]
    fn yim_yanks_a_message_or_an_answer_whole_and_yam_its_turn() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "**bold** answer")]));
        read(&mut chat, "yim");
        assert_eq!(yanked(&mut chat, &mut fonts), [lines("**bold** answer\n")]);
        read(&mut chat, "ggyim");
        assert_eq!(yanked(&mut chat, &mut fonts), [lines("one\n")]);
        read(&mut chat, "yam");
        let turn = yanked(&mut chat, &mut fonts).remove(0).0;
        assert!(
            turn.contains("one") && turn.contains("**bold** answer"),
            "{turn}"
        );
    }

    #[test]
    fn u_undoes_the_turn_under_the_cursor_or_else_the_last() {
        use clackatoo_protocol::{ChatAction, TurnFiles, TurnFilesState};

        let mut node = talk(&[("one", "first"), ("two", "second"), ("three", "third")]);
        for turn in &mut node.turns[..2] {
            turn.files = Some(TurnFiles {
                count: 1,
                state: TurnFilesState::Made,
            });
        }
        let (mut chat, mut fonts) = reading(node);
        let undo = |turn| ChatAction::Undo { turn, force: false };
        // On the last turn, which changed nothing: the last that did.
        read(&mut chat, "u");
        assert_eq!(chat.settle(&mut fonts).actions, [undo(1)]);
        // It rewrites files: the bar says whose.
        assert_eq!(chat.label().as_deref(), Some("Undoing turn 2 (1 file)"));
        read(&mut chat, "ggu");
        assert_eq!(chat.settle(&mut fonts).actions, [undo(0)]);
        // The cursor's turn out of view, as scrolled away from: the last.
        chat.shown.retain(|shown| shown.key.turn != 0);
        read(&mut chat, "u");
        assert_eq!(chat.settle(&mut fonts).actions, [undo(1)]);
        // Nothing to redo: the bar says so, until the next key.
        let ctrl_r = |chat: &mut ChatView| {
            chat.read_key(&crate::input::KeyPress {
                key: &winit::keyboard::Key::Character("r".into()),
                text: None,
                location: winit::keyboard::KeyLocation::Standard,
                code: None,
                modifiers: crate::input::Modifiers {
                    ctrl: true,
                    ..crate::input::Modifiers::default()
                },
            })
        };
        assert_eq!(ctrl_r(&mut chat), (true, false));
        assert_eq!(chat.label().as_deref(), Some("no turn has files to redo"));
        read(&mut chat, "j");
        assert_eq!(chat.label(), None);
    }

    #[test]
    fn keys_are_read_by_the_grammar_with_counts_finds_and_words_of_anything_but_blanks() {
        let (mut chat, mut fonts) =
            reading(talk(&[("one", "alpha beta.gamma delta, epsilon zeta")]));
        read(&mut chat, "G0");
        let byte = |chat: &ChatView| chat.cursor.unwrap().byte;
        assert_eq!(byte(&chat), 0);
        // A count, shown in the bar while it's typed.
        read(&mut chat, "3");
        assert_eq!(chat.label().as_deref(), Some("3"));
        read(&mut chat, "w");
        assert_eq!(byte(&chat), "alpha beta.".len());
        read(&mut chat, "0W");
        assert_eq!(byte(&chat), "alpha ".len());
        read(&mut chat, "E");
        assert_eq!(byte(&chat), "alpha beta.gamma".len() - 1);
        read(&mut chat, "B");
        assert_eq!(byte(&chat), "alpha ".len());
        // `f`, `t`, `;` and `,` on the line, a count finding that many.
        read(&mut chat, "0fa");
        assert_eq!(byte(&chat), "alph".len());
        read(&mut chat, "2fa");
        assert_eq!(byte(&chat), "alpha beta.g".len());
        read(&mut chat, "0te");
        assert_eq!(byte(&chat), "alpha b".len() - 1);
        read(&mut chat, ";");
        assert_eq!(byte(&chat), "alpha beta.gamma d".len() - 1);
        read(&mut chat, ",");
        assert_eq!(byte(&chat), "alpha be".len());
        // Ctrl+E with a count is the window's to scroll; a count before a
        // key a chat doesn't take, as `d`, spends it.
        read(&mut chat, "2d");
        assert!(chat.keys().pending());
        read(&mut chat, "w");
        assert!(!chat.keys().pending());
        paint(&mut chat, &mut fonts);
        let rows = chat.shortcut_context(crate::vim::Mode::Normal).rows;
        let has = |key: &str| rows.iter().any(|(k, _)| k == key);
        assert!(has("za") && has("zL") && has("W") && has("K"), "{rows:?}");
        assert!(has("f + character") && !has("O"), "{rows:?}");
        assert!(rows.contains(&("o".into(), "Compose a message".into())));
        assert!(!has("dd") && !has("p"), "nothing that changes text");
    }

    #[test]
    fn semicolon_finds_again_what_another_pane_of_the_window_found() {
        use crate::motion::Motion;

        let (mut chat, _fonts) = reading(talk(&[("one", "alpha beta")]));
        let window = crate::vim::Finds::default();
        chat.share_finds(&window);
        read(&mut chat, "G0");
        // `fb` in an editor of the window.
        let find = |ch, till, again| Motion::Find {
            ch,
            forward: true,
            till,
            again,
        };
        window.remember(find('b', false, false));
        read(&mut chat, ";");
        assert_eq!(chat.cursor.unwrap().byte, "alpha ".len());
        // And `ta` here is the window's.
        read(&mut chat, "0ta");
        assert_eq!(window.again(false), Some(find('a', true, true)));
    }

    #[test]
    fn the_scrolling_keys_take_counts_and_go_as_far_as_in_any_pane() {
        let (mut chat, mut fonts) = reading(crate::chat::fixtures::long_chat(12));
        let ctrl = |chat: &mut ChatView, key: &str| {
            chat.read_key(&crate::input::KeyPress {
                key: &winit::keyboard::Key::Character(key.into()),
                text: None,
                location: winit::keyboard::KeyLocation::Standard,
                code: None,
                modifiers: crate::input::Modifiers {
                    ctrl: true,
                    ..crate::input::Modifiers::default()
                },
            })
        };
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        let top = chat.scroll;
        ctrl(&mut chat, "e");
        let line = chat.scroll - top;
        assert!(line > 0.0);
        read(&mut chat, "3");
        ctrl(&mut chat, "e");
        assert!(
            (chat.scroll - top - 4.0 * line).abs() < 0.5,
            "four lines in all"
        );
        // A page is the view less two lines, as in any pane.
        let before = chat.scroll;
        read_named(&mut chat, NamedKey::PageDown);
        let rows = (chat.view_height / chat.line_height).floor();
        assert!(
            (chat.scroll - before - (rows - 2.0) * chat.line_height).abs() < 0.5,
            "{} {} {} {} {}",
            chat.scroll,
            before,
            rows,
            chat.line_height,
            chat.max_scroll
        );
    }

    #[test]
    fn ctrl_with_shift_scrolls_as_ctrl_does() {
        let (mut chat, mut fonts) = reading(crate::chat::fixtures::long_chat(12));
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        let top = chat.scroll;
        chat.read_key(&crate::input::KeyPress {
            key: &winit::keyboard::Key::Character("E".into()),
            text: None,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers: crate::input::Modifiers {
                ctrl: true,
                shift: true,
                ..crate::input::Modifiers::default()
            },
        });
        assert!(chat.scroll > top, "a line down");
    }

    #[test]
    fn a_count_before_gg_or_g_goes_to_that_turn() {
        let (mut chat, _fonts) = reading(talk(&[("one", "a"), ("two", "b"), ("three", "c")]));
        let turn = |chat: &ChatView| chat.cursor.unwrap().text.turn;
        read(&mut chat, "2G");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(1, PROMPT)));
        read(&mut chat, "1gg");
        assert_eq!(turn(&chat), 0);
        read(&mut chat, "9G");
        assert_eq!(turn(&chat), 2, "the last, with no ninth");
    }

    #[test]
    fn a_turns_number_counts_the_turns_not_loaded_yet() {
        let mut node = talk(&[("eleven", "a"), ("twelve", "b"), ("thirteen", "c")]);
        node.first_message = 10;
        let (mut chat, _fonts) = reading(node);
        let turn = |chat: &ChatView| chat.cursor.unwrap().text.turn;
        read(&mut chat, "12G");
        assert_eq!(turn(&chat), 1, "the twelfth, the second loaded");
        read(&mut chat, "3G");
        assert_eq!(turn(&chat), 0, "before those loaded: the first");
        read(&mut chat, "G11gg");
        assert_eq!(turn(&chat), 0);
    }

    #[test]
    fn a_new_height_forgets_how_far_a_count_set_ctrl_d_to_go() {
        let (mut chat, mut fonts) = reading(crate::chat::fixtures::long_chat(12));
        let ctrl_d = |chat: &mut ChatView| {
            chat.read_key(&crate::input::KeyPress {
                key: &winit::keyboard::Key::Character("d".into()),
                text: None,
                location: winit::keyboard::KeyLocation::Standard,
                code: None,
                modifiers: crate::input::Modifiers {
                    ctrl: true,
                    ..crate::input::Modifiers::default()
                },
            })
        };
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        let before = chat.scroll;
        read(&mut chat, "2");
        ctrl_d(&mut chat);
        let two = chat.scroll - before;
        assert!((two - 2.0 * chat.line_height).abs() < 0.5, "two lines");
        // Kept, as Vim's `scroll` is, until the view's height changes.
        let before = chat.scroll;
        ctrl_d(&mut chat);
        assert!((chat.scroll - before - two).abs() < 0.5);
        chat.paint(&mut fonts, [0.0; 2], [900.0, 700.0], true);
        let before = chat.scroll;
        ctrl_d(&mut chat);
        assert!(
            chat.scroll - before > 4.0 * chat.line_height,
            "half the view again"
        );
    }

    #[test]
    fn words_are_found_as_vim_finds_them() {
        let text = "fn main() {  x }";
        assert_eq!(next_word_start(text, 0, class), Some(3));
        assert_eq!(next_word_start(text, 3, class), Some(7));
        assert_eq!(next_word_start(text, 10, class), Some(13));
        assert_eq!(word_end(text, 0, class), Some(1));
        assert_eq!(word_end(text, 1, class), Some(6));
        assert_eq!(word_start_before(text, 7, class), Some(3));
        assert_eq!(word_start_before(text, 5, class), Some(3));
        assert_eq!(word_start_before(text, 0, class), None);
        assert_eq!(next_word_start("end", 0, class), None);
        // WORDs: anything between blanks.
        assert_eq!(next_word_start(text, 0, big_class), Some(3));
        assert_eq!(next_word_start(text, 3, big_class), Some(10));
        assert_eq!(word_end(text, 3, big_class), Some(8));
    }
}
