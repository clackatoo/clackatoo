//! The input shell shared by explorer, board and review panes.

use super::{Searched, Searching, go, go_own, search_command, search_stroke, strokes};
use crate::motion::Motion;
use crate::vim::{Action, Command, Dialect, Paging, Parsed, Stroke, search};

/// A list pane's key handling. Its dialect supplies its own actions; the
/// shell routes prompts and searches before reading normal-mode commands.
/// All dispatch is static, and paging stays with the pane across commands.
pub(crate) trait ListPane: Searched + Sized {
    type Dialect: Dialect;

    fn dialect() -> Self::Dialect;
    fn paging(&mut self) -> &mut Paging;
    /// Clears the last key's message, returning whether one was shown.
    fn clear_message(&mut self) -> bool;
    fn own(&mut self, key: <Self::Dialect as Dialect>::Own, count: Option<usize>);

    /// Settles pane-specific state before and after each stroke. The
    /// explorer uses these to keep its flattened tree current.
    fn before_stroke(&mut self) {}
    fn after_stroke(&mut self) {}

    /// Takes a key for a pane-specific prompt or edit plan, if one is open.
    fn prompt_stroke(&mut self, _stroke: Stroke) -> bool {
        false
    }

    /// Reads one stroke and reports whether the pane needs redrawing.
    fn stroke(&mut self, stroke: Stroke) -> bool {
        self.before_stroke();
        let had_message = self.clear_message();
        let redraw = if self.prompt_stroke(stroke) || search_stroke(self, stroke) {
            true
        } else {
            let was_pending = self.keys().pending();
            match self.keys_mut().read(stroke, false, &Self::dialect()) {
                Parsed::Incomplete => true,
                Parsed::Nothing => was_pending || had_message,
                Parsed::Command(command) => {
                    self.execute(command);
                    true
                }
            }
        };
        self.after_stroke();
        redraw
    }

    /// Runs shared movement and search commands before pane-specific ones.
    fn execute(&mut self, command: Command<<Self::Dialect as Dialect>::Own>) {
        if self.keys_mut().window(&command) {
            return;
        }
        let mut paging = *self.paging();
        let from = self.cursor();
        let moved = go(self, &mut paging, &command);
        if moved && command.jumps() && from != self.cursor() {
            self.remember_jump(from);
        }
        *self.paging() = paging;
        if moved || search_command(self, &command) {
            return;
        }
        if let Action::Own(key) = command.action {
            self.own(key, command.count);
            if from != self.cursor() {
                self.remember_jump(from);
            }
        }
    }

    /// Home/End and Ctrl+N/P, whose count rules differ from Vim motions.
    fn move_by(&mut self, motion: Motion, count: Option<usize>) {
        let mut paging = *self.paging();
        go_own(self, &mut paging, motion, count);
        *self.paging() = paging;
    }

    /// Drops a half-typed command and restores an unfinished search's
    /// cursor and view. Pane-specific drafts and confirmations stay open.
    fn leave(&mut self) {
        self.keys_mut().clear();
        search::give_up(&mut Searching(self));
    }
}

/// Hands every stroke of a physical key press to the pane. Do not stop
/// after a stroke requests a redraw: composed text can contain several.
pub(crate) fn press(pane: &mut impl ListPane, press: &crate::input::KeyPress<'_>) -> bool {
    let mut redraw = false;
    for stroke in strokes(press, pane) {
        redraw |= pane.stroke(stroke);
    }
    redraw
}

/// Edits a pane's text prompt, returning the accepted prompt once. Escape
/// discards it; unfinished text stays in place. Confirmations use their
/// own answer rules and never enter this path.
pub(crate) fn prompt_key<P>(
    prompt: &mut Option<P>,
    stroke: Stroke,
    text: impl FnOnce(&mut P) -> Option<&mut String>,
) -> Option<P> {
    use crate::cmdline::simple::{Outcome, key};
    match key(text(prompt.as_mut()?)?, stroke) {
        Outcome::Accept => prompt.take(),
        Outcome::Cancel => {
            *prompt = None;
            None
        }
        _ => None,
    }
}
