//! Macros in the desk: the keys pressed while one is recorded, and typing
//! them again, as Vim's `q` and `@` do.
//!
//! **Recording** keeps every key pressed, as it was pressed, whatever has
//! it: an editor, a terminal, the command line, the finder, or one of the
//! window's or the config's bindings. So a macro moves between panes with
//! Ctrl+L, runs `:w`, and types into the finder. Keys are kept before the
//! bindings see them, as Vim keeps them before mappings, and typed again
//! through the bindings, so `@a` is as if its keys were pressed now. The
//! one exception is a key a completion menu took, which is kept as what
//! it put in: a macro never asks the language server, so the menu won't
//! be there when it's typed again. The `q` that stops recording isn't
//! kept. `q` stops it in an editor's normal mode, and in a terminal's.
//!
//! **Typing** a macro pushes its keys on a stack of frames, which the desk
//! hands to [`key_with`](Desk::key_with) one at a time, so each key is
//! seen to before the next, wherever it goes. Keys a binding types go on
//! the same stack, past the config's bindings, so a binding that types
//! `@qx` types q's keys and then `x`, as in Vim. A frame is popped before
//! its last key is handed over, so a macro that types itself last goes no
//! deeper.
//!
//! **Waiting:** before each key a macro types, the desk checks whether
//! the keys before it started something the next may depend on: an
//! answer from the daemon or the language server, a file opening, or the
//! finder's search, when the key needs what it finds. If so, the macro
//! waits, and keys pressed meanwhile wait behind it, as they wait for an
//! answer from the grammar. It waits only for what it asked, not for
//! anything older, which may never be answered. It gives up after
//! [`PATIENCE`], or at Esc, or if the focus leaves the pane it waits in,
//! as a click or another window closing the pane moves it: its keys were
//! for that pane. The focus moving as it asked, as to a pane it made, is
//! where it goes on. Keys a binding types outside a macro don't wait, as
//! they never have; a binding a macro's key sets off is part of the macro.
//!
//! **Stopping:** a key that fails stops every frame: an editor's motion
//! that can't move, a command that isn't one or is refused, Enter in the
//! finder with nothing picked, or the daemon refusing what was asked.
//!
//! **Undo:** each file a macro changes gets one undo step for all of it
//! (see [`BufferCopy::first_macro_edit`]).

use std::rc::Rc;
use std::time::{Duration, Instant};

use clackatoo_protocol::PaneId;

use super::{Asked, Asker, Desk};
use crate::editor::BufferCopy;
use crate::input::{KeptPress, KeyPress};
use crate::keymap::{Base, Chord, Named};
use crate::vim::Stroke;

/// The most keys macros type before they're stopped: one that types
/// itself again, and never fails, would go on for ever. Fewer in tests,
/// which run unoptimised.
const MOST_TYPED: usize = if cfg!(test) { 20_000 } else { 1_000_000 };

/// The deepest macros and bindings type each other, as Vim's
/// `maxmapdepth`.
const DEEPEST: usize = 1000;

/// How long a macro waits for something its keys started before it's
/// stopped. Longer than a command waits for the grammar, as a language
/// server can take a while to answer.
pub(super) const PATIENCE: Duration = Duration::from_secs(5);

/// The macros, and keys bindings type, being typed, and what's noted of
/// the key being pressed for the macro being recorded.
#[derive(Debug, Default)]
pub(super) struct Macros {
    /// The keys being typed, the frame typed last on top.
    frames: Vec<Frame>,
    /// How many keys they've typed so far.
    typed: usize,
    /// What the desk is handing over, while it hands over keys.
    handing: Option<Handing>,
    /// Since when a macro has waited for something its keys started, and
    /// what, and the pane that had the focus then: if the focus leaves it
    /// meanwhile, other than as the macro asked, the macro stops, as its
    /// keys were for that pane.
    paused: Option<(Instant, &'static str)>,
    waiting_in: Option<PaneId>,
    /// The first query, and the first of the window's requests, that the
    /// macro being typed could have made: it waits only for those, not
    /// for any older, which may never be answered.
    since: (u64, u64),
    /// How many of the window's requests a macro would wait for have been
    /// sent, and how many answered.
    requests: (u64, u64),
    /// Whether a macro has been typed since the last was done, whose
    /// edits started undo steps in the buffers.
    edited: bool,
    /// While a key is pressed and recorded: what the editors were handed
    /// for it, as keys, and whether a menu took any of them, so it's
    /// noted as what the menu put in instead.
    pressed: Option<Pressed>,
}

/// What the editors were handed for a key pressed and recorded.
#[derive(Debug, Default)]
struct Pressed {
    keys: Vec<Chord>,
    /// A completion menu took some of it.
    took: bool,
    /// Some of it came from a binding the key set off, so it's noted as
    /// the key pressed, whatever the menu took.
    bound: bool,
}

/// What the desk is handing over, while it hands over keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handing {
    /// A key held while something waited, or nothing just now, between
    /// keys.
    Held,
    /// A key a binding types, outside any macro.
    Binding,
    /// A key a macro types, or a binding a macro set off.
    Macro,
}

/// The keys of a macro, or of a binding, being typed, as many `times`
/// over again as are left after this, and how far through them it's got.
#[derive(Debug)]
struct Frame {
    keys: Rc<[KeptPress]>,
    next: usize,
    times: usize,
    /// Typed by a binding, past the config's bindings, as Vim's `noremap`
    /// would; or else a macro's, typed through them.
    typing: bool,
    /// Part of a macro: a macro's own keys, or a binding a macro's key
    /// set off.
    in_macro: bool,
}

impl Macros {
    /// Whether a macro is being typed, and waits for something.
    fn waiting(&self) -> bool {
        !self.frames.is_empty() && self.handing.is_none()
    }

    /// Whether there's a macro among the frames.
    fn in_macro(&self) -> bool {
        self.frames.iter().any(|frame| frame.in_macro)
    }

    /// Notes a request sent by the window, which `asker` asked.
    pub(super) fn requested(&mut self, asker: &Asker) {
        if waits_for(asker) {
            self.requests.0 += 1;
        }
    }

    /// Notes the daemon's answer to what `asker` asked.
    pub(super) fn request_answered(&mut self, asker: &Asker) {
        if waits_for(asker) {
            self.requests.1 += 1;
        }
    }

    /// Forgets the requests sent, which will never be answered, as when
    /// the connection is lost.
    pub(super) fn requests_forgotten(&mut self) {
        self.requests.1 = self.requests.0;
    }

    /// Whether a request the macro being typed made isn't answered yet.
    /// They're answered in the order they were sent, so the newest are
    /// those left.
    fn request_waiting(&self) -> bool {
        let (sent, answered) = self.requests;
        sent > answered.max(self.since.1)
    }

    /// Takes the next key to type: its frame's keys, which key it is, and
    /// whether it's typed past the config's bindings.
    fn take(&mut self) -> Option<(Rc<[KeptPress]>, usize, bool)> {
        let frame = self.frames.last_mut()?;
        let (keys, index, typing) = (Rc::clone(&frame.keys), frame.next, frame.typing);
        frame.next += 1;
        if frame.next == frame.keys.len() {
            if frame.times > 1 {
                frame.times -= 1;
                frame.next = 0;
            } else {
                self.frames.pop();
            }
        }
        self.typed += 1;
        Some((keys, index, typing))
    }
}

impl Desk {
    /// Notes `press` in the macro being recorded, if one is, before it's
    /// handled: a character as the text it types, so Shift and an input
    /// method's text are right, and any other key as itself.
    pub(super) fn record_press(&mut self, press: &KeyPress<'_>) {
        let registers = &mut self.shared_mut().registers;
        if registers.recording().is_none() {
            self.macros.pressed = None;
            return;
        }
        let modifiers = press.modifiers;
        let typed = press
            .text
            .filter(|_| !modifiers.ctrl && !modifiers.alt)
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
        let keys: Vec<Chord> = match (press.key, typed) {
            (winit::keyboard::Key::Named(_), _) | (_, None) => {
                Chord::of(press).into_iter().collect()
            }
            (_, Some(text)) => text.chars().map(Chord::typed).collect(),
        };
        registers.pressed(keys);
        self.macros.pressed = Some(Pressed::default());
    }

    /// The key pressed has been handled: if a completion menu took any of
    /// what it typed, the macro being recorded notes what the editors
    /// were handed instead, the menu's keys as what they put in.
    pub(super) fn recorded_press(&mut self) {
        if let Some(Pressed {
            keys,
            took: true,
            bound: false,
        }) = self.macros.pressed.take()
        {
            self.shared_mut().registers.pressed_instead(keys);
        }
    }

    /// Notes that an editor was handed `stroke`, or if a menu took it,
    /// what it put in, `menu`, for [`recorded_press`](Self::recorded_press).
    pub(super) fn handed_to_editor(&mut self, stroke: Stroke, menu: Option<Vec<Stroke>>) {
        if self.replaying() {
            return;
        }
        let bound = self.key_from_frame();
        let Some(pressed) = &mut self.macros.pressed else {
            return;
        };
        pressed.bound |= bound;
        match menu {
            Some(put) => {
                pressed.took = true;
                pressed.keys.extend(put.into_iter().map(Stroke::chord));
            }
            None => pressed.keys.push(stroke.chord()),
        }
    }

    /// Whether the key being handed to a pane comes from a macro.
    pub(super) fn replaying(&self) -> bool {
        self.macros.handing == Some(Handing::Macro)
    }

    /// Types `keys`, a macro `@` asked for, `times` over, after the key
    /// being handled.
    pub(super) fn type_macro(&mut self, keys: &[Chord], times: usize) {
        // A macro typed afresh waits only for what it asks from now on.
        if !self.macros.in_macro() && !self.replaying() {
            self.macros.since = (crate::editor::queries_asked(), self.macros.requests.0);
        }
        self.push_frame(keys, times, false);
        self.macros.edited = true;
    }

    /// Types `keys`, as a binding to keys does: each as if pressed, so the
    /// window's own keys do what they do (`:` opens the command line), but
    /// the config's bindings and keypress handlers don't see them, so a
    /// binding can't set off itself.
    pub(super) fn type_keys(&mut self, keys: &[Chord]) {
        self.push_frame(keys, 1, true);
    }

    fn push_frame(&mut self, keys: &[Chord], times: usize, typing: bool) {
        if keys.is_empty() || times == 0 {
            return;
        }
        if self.macros.frames.len() >= DEEPEST {
            self.stop_macro(Some(format!(
                "macros and bindings typed each other {DEEPEST} deep, so they were stopped"
            )));
            return;
        }
        // A binding a macro's key sets off is part of the macro: it
        // waits, stops and is undone with it.
        let in_macro = !typing || self.replaying();
        self.macros.frames.push(Frame {
            keys: keys.iter().map(|key| key.kept()).collect(),
            next: 0,
            times,
            typing,
            in_macro,
        });
    }

    /// Stops the macros, and keys bindings type, being typed, saying `why`
    /// if there's something to say: a key failed.
    pub(super) fn stop_macro(&mut self, why: Option<String>) {
        // What it half typed goes too: a binding waiting for its next
        // key, or a command in the focused editor. Not a held key's, which
        // was pressed.
        if self.macros.in_macro() || self.key_from_frame() {
            self.cancel_keys();
            if let Some(pane) = self.shown.map(|shown| shown.pane) {
                self.forget_typed_in(pane);
            }
        }
        self.macros.frames.clear();
        self.macros.paused = None;
        self.macros.waiting_in = None;
        if let Some(why) = why {
            self.say(why);
        }
    }

    /// `message` says why what a key asked for can't be done, such as a
    /// command that isn't one: it's said, and the macro typing the key
    /// stops, as in Vim.
    pub(super) fn refuse(&mut self, message: String) {
        self.warn(message);
        self.stop_macro(None);
    }

    /// Hands over the keys of the macros and bindings being typed, one at
    /// a time, and then the keys held while they or an editor waited,
    /// until none are left, or something must be waited for. Returns
    /// whether the window must be redrawn.
    pub(super) fn type_macros(&mut self) -> bool {
        if self.macros.handing.is_some() {
            return false;
        }
        // An editor's text come, the keys held for it go in insert mode.
        self.insert_once_loaded();
        // Not if the focus has left the pane it waited in.
        let mut redraw = self.give_up_off_focus();
        self.macros.handing = Some(Handing::Held);
        loop {
            // A terminal's answer come, nothing waits for it any more.
            if !self.pane_waits_to_settle() {
                self.pane_wait = None;
            }
            if self.editors_wait_for_grammar() || self.pane_waits_to_settle() {
                if self.pane_waits_to_settle() {
                    self.note_pane_wait(Instant::now());
                }
                self.macros.waiting_in = self.shown.map(|shown| shown.pane);
                break;
            }
            if self.macros.frames.is_empty() {
                self.macro_done();
                // Held for an editor's text, they wait for it, and go only
                // to it.
                self.keep_held_with_their_editor();
                if !self.waits_to_insert() && self.have_held() {
                    redraw = true;
                    continue;
                }
                break;
            }
            let replaying = self.macros.in_macro();
            if replaying && let Some(what) = self.macro_waits_for() {
                let since = self
                    .macros
                    .paused
                    .map_or_else(Instant::now, |(since, _)| since);
                self.macros.paused = Some((since, what));
                self.macros.waiting_in = self.shown.map(|shown| shown.pane);
                break;
            }
            (self.macros.paused, self.macros.waiting_in) = (None, None);
            if self.macros.typed >= MOST_TYPED {
                self.stop_macro(Some(format!(
                    "the macro was stopped after {MOST_TYPED} keys: it may type itself for ever"
                )));
                continue;
            }
            let Some((keys, index, typing)) = self.macros.take() else {
                continue;
            };
            let was = std::mem::replace(&mut self.script.typing, typing);
            self.macros.handing = Some(if replaying {
                Handing::Macro
            } else {
                Handing::Binding
            });
            redraw |= self.key_with(&keys[index].press(), true);
            self.macros.handing = Some(Handing::Held);
            self.script.typing = was;
            // A chat's yank or a terminal's motion that failed stops it,
            // as Vim's failing command does; a search fails once settled
            // or answered.
            if self.take_pane_failed() {
                self.stop_macro(None);
            }
        }
        self.macros.handing = None;
        redraw
    }

    /// The macros being typed are done, and nothing waits: the buffers
    /// they edited start a new undo step with the next.
    fn macro_done(&mut self) {
        // A selection made from insert mode with the mouse that a macro or
        // a binding ended goes back there now, so their keys weren't typed
        // into it.
        if let Some(pane) = self.shown.map(|shown| shown.pane) {
            self.with_editor(pane, |editor, _| editor.back_to_insert());
        }
        self.macros.typed = 0;
        self.macros.paused = None;
        self.macros.waiting_in = None;
        if std::mem::take(&mut self.macros.edited) {
            self.each_copy(BufferCopy::macro_done);
        }
    }

    /// What the next key of a macro must wait for, if anything, which the
    /// keys before it started: the daemon's answer to the window, the
    /// focused pane's file to open and its text to come, the language
    /// server or the daemon to answer its editor, formatting, or the
    /// finder's search, if the key needs what it finds.
    fn macro_waits_for(&mut self) -> Option<&'static str> {
        if self.macros.request_waiting() {
            return Some("the daemon");
        }
        let since = self.macros.since.0;
        if let Some(pane) = self.shown.map(|shown| shown.pane)
            && let Some(what) = self.editor_waits_for(pane, since)
        {
            return Some(what);
        }
        if self.formatting.keys().any(|&query| query >= since) {
            return Some("formatting");
        }
        self.finder_waits().then_some("the finder")
    }

    /// Whether the finder is open and still searching, and the next key
    /// needs what it finds: it doesn't just type into it. A grep a macro
    /// types searches at once, rather than once typing pauses.
    fn finder_waits(&mut self) -> bool {
        if self.finder.is_none() {
            return false;
        }
        let next = (self.macros.frames.last())
            .and_then(|frame| Chord::of(&frame.keys[frame.next].press()));
        if next.is_some_and(types_text) {
            return false;
        }
        if self.grep_at.is_some() {
            self.grep_now();
        }
        (self.finder.as_ref())
            .is_some_and(|finder| finder.search().is_some() || finder.asking().is_some())
    }

    /// Whether a macro being typed waits for something, so keys pressed
    /// meanwhile wait too.
    pub(super) fn macro_waiting(&self) -> bool {
        self.macros.waiting()
    }

    /// Whether a macro or a binding is being typed, or waits to go on.
    pub(super) fn frames_typing(&self) -> bool {
        !self.macros.frames.is_empty()
    }

    /// Whether the key being handled comes from a macro or a binding
    /// being typed: an Esc among them doesn't stop what waits, as one
    /// pressed does.
    pub(super) fn key_from_frame(&self) -> bool {
        matches!(self.macros.handing, Some(Handing::Binding | Handing::Macro))
    }

    /// Stops a macro that has waited too long, by `now`, saying what for,
    /// with the keys held behind it. Returns whether it did.
    pub(super) fn macro_patience(&mut self, now: Instant) -> bool {
        let Some((since, what)) = self.macros.paused else {
            return false;
        };
        if now < since + PATIENCE {
            return false;
        }
        self.stop_macro(Some(format!(
            "the macro was stopped: it waited too long for {what}"
        )));
        self.drop_held();
        true
    }

    /// `q` in normal mode, with nothing half typed or being typed, stops
    /// the macro being recorded, as it does in an editor's, in a terminal
    /// and in an explorer, a board or a review, rather than closing it.
    /// An editor reads its own `q`; a chat's isn't one. Returns whether it
    /// did.
    pub(super) fn stop_recording_in_pane(&mut self, press: &KeyPress<'_>) -> bool {
        if self.recording().is_none() || Chord::of(press) != Some(crate::vim::RECORD_STOP.chord()) {
            return false;
        }
        let chat = self.focused_chat().is_some();
        let normal = !chat
            && self.focused_editor().is_none()
            && self.focused_key_state().is_some_and(|keys| {
                keys.mode() == crate::vim::Mode::Normal && !keys.prompting() && !keys.pending()
            });
        if normal {
            self.shared_mut().registers.stop_recording();
        }
        normal
    }

    /// Whether a macro waiting has seen the focus leave the pane it
    /// waited in, and so is to stop.
    pub(super) fn macro_left_behind(&self) -> bool {
        let focused = self.shown.map(|shown| shown.pane);
        self.macros.waiting() && (self.macros.waiting_in).is_some_and(|pane| Some(pane) != focused)
    }

    /// Whether a macro waits for the daemon to answer a request it made,
    /// which may move the focus, as to a pane it asked for.
    pub(super) fn macro_asked(&self) -> bool {
        self.macros.waiting() && self.macros.request_waiting()
    }

    /// The focus has moved as a macro waiting asked: it goes on in the
    /// pane that has it now.
    pub(super) fn macro_follows_focus(&mut self) {
        self.macros.waiting_in = self.shown.map(|shown| shown.pane);
    }

    /// When a macro waiting is to give up, if one is.
    pub(super) fn macro_gives_up(&self) -> Option<Instant> {
        self.macros.paused.map(|(since, _)| since + PATIENCE)
    }
}

/// Whether a macro waits for the daemon to answer what `asker` asked: one
/// of the window's requests, but for a split resized, or one whose
/// failure is only logged.
pub(super) fn waits_for(asker: &Asker) -> bool {
    matches!(asker, Asker::Window(what) if !matches!(what, Asked::Quiet | Asked::Resize | Asked::Workspaces | Asked::Reattached(_)))
}

/// Whether `key` types into the finder's query, and so needn't wait for
/// what it finds: a character, Backspace, Ctrl+U or Ctrl+W.
fn types_text(key: Chord) -> bool {
    match key.base {
        Base::Char(ch) if key.ctrl && !key.alt => matches!(ch, 'u' | 'w'),
        Base::Char(_) => !key.alt,
        Base::Named(Named::Space | Named::Backspace) => !key.ctrl && !key.alt,
        Base::Named(_) => false,
    }
}
