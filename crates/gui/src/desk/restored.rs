//! What panes the daemon made again as it started have to show for it
//! (see [`Workspaces::restored`]): a terminal whose shell was running a
//! command when the daemon last stopped offers to run it again.
//!
//! The offer is a small popup at the terminal's bottom right, "was running:
//! cargo watch · R puts it at the prompt · Esc dismiss". In normal mode,
//! `R` types the command at the new shell's prompt, as if typed, without
//! Enter, so the whole command is there to read before it's run; and Esc
//! dismisses it, as well as doing what Esc does. A click on either does
//! the same. Neither the window nor the daemon ever runs it. `R` does
//! nothing while the offer isn't drawn, as in a pane too narrow for it,
//! nor from a macro or a binding. The offer goes, in every window, once
//! it's taken or dismissed, once anything is typed into the terminal or
//! its shell runs any command, or once the pane closes: the daemon says
//! so. An editor's place, the other thing a pane may have to show, is put
//! back by `editors`.
//!
//! [`Workspaces::restored`]: clackatoo_protocol::Workspaces::restored

use std::ops::Range;

use clackatoo_protocol::{ClientMessage, PaneId, TerminalId, wire};
use winit::keyboard::{Key, NamedKey};

use super::Desk;
use crate::input::KeyPress;
use crate::render::mono_width;
use crate::vim::Mode;

/// What the offer says after the command, in full, and in a pane too
/// narrow for that.
const KEYS: [[&str; 2]; 2] = [["R puts it at the prompt", "Esc dismiss"], ["R", "Esc"]];

/// What goes between its parts.
const BETWEEN: &str = " · ";

/// What it says before the command.
const RAN: &str = "was running: ";

/// The fewest characters of a command shown, cut short, before the keys
/// are said shortly, or the offer is left out of a pane too narrow for it.
const SHORTEST: usize = 6;

/// The offer to run a command again, laid out for a terminal's cells: on
/// its last row, out of the way of the new shell's prompt at its top, a
/// cell in from its right edge, with a cell of room either side of its
/// text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    /// What it says.
    pub(crate) text: String,
    /// Where the command is in the text, in bytes.
    pub(crate) command: Range<usize>,
    /// The row it's on.
    pub(crate) row: u16,
    /// The column its box starts at; its text starts a column after.
    pub(crate) col: u16,
    /// The columns a click puts it at the prompt in.
    put: Range<u16>,
    /// The columns a click dismisses it in.
    dismiss: Range<u16>,
}

impl Offer {
    /// The offer to run `command` again, in a terminal `cols` wide and
    /// `rows` high, the command cut short as needs be, and the keys said
    /// shortly if they must: `None` if it won't fit at all.
    pub(crate) fn new(command: &str, cols: u16, rows: u16) -> Option<Self> {
        let row = rows.checked_sub(1)?;
        // The box's room either side, and a cell from the edge.
        let room = usize::from(cols).checked_sub(3)?;
        KEYS.iter()
            .find_map(|keys| Self::laid_out(command, keys, room, cols, row))
    }

    /// The offer, saying `keys`, if the command shows in `room` cells
    /// beside them, or enough of it.
    fn laid_out(command: &str, keys: &[&str; 2], room: usize, cols: u16, row: u16) -> Option<Self> {
        let said = keys.join(BETWEEN);
        let fixed = mono_width(RAN) + mono_width(BETWEEN) + mono_width(&said);
        let left = room.checked_sub(fixed)?;
        let shown = if mono_width(command) <= left {
            command.to_owned()
        } else {
            let mut cut = String::new();
            for ch in command.chars() {
                if mono_width(&cut) + mono_width(&ch.to_string()) + 1 > left {
                    break;
                }
                cut.push(ch);
            }
            if cut.chars().count() < SHORTEST {
                return None;
            }
            cut.push('…');
            cut
        };
        let text = format!("{RAN}{shown}{BETWEEN}{said}");
        let command = RAN.len()..RAN.len() + shown.len();
        let width = u16::try_from(mono_width(&text)).ok()?;
        let col = cols - width - 3;
        let at = |cells: usize| col + 1 + u16::try_from(cells).unwrap_or(u16::MAX);
        let put_start = mono_width(RAN) + mono_width(&shown) + mono_width(BETWEEN);
        let put_end = put_start + mono_width(keys[0]);
        let dismiss_start = put_end + mono_width(BETWEEN);
        Some(Self {
            text,
            command,
            row,
            col,
            put: at(put_start)..at(put_end),
            dismiss: at(dismiss_start)..at(dismiss_start + mono_width(keys[1])),
        })
    }

    /// How many cells wide its box is: its text, and a cell of room
    /// either side.
    pub(crate) fn width(&self) -> u16 {
        u16::try_from(mono_width(&self.text) + 2).unwrap_or(u16::MAX)
    }
}

/// What a click on an offer does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clicked {
    Put,
    Dismiss,
    /// Nothing: it's on the offer's words.
    Nothing,
}

impl Offer {
    /// What a click on the cell at `col` of its row does, if it's on the
    /// offer.
    fn clicked(&self, col: u16) -> Option<Clicked> {
        if self.put.contains(&col) {
            Some(Clicked::Put)
        } else if self.dismiss.contains(&col) {
            Some(Clicked::Dismiss)
        } else {
            (self.col..self.col + self.width())
                .contains(&col)
                .then_some(Clicked::Nothing)
        }
    }
}

impl Desk {
    /// The command the pane showing `terminal` offers to run again, if
    /// it does.
    fn ran_in(&self, terminal: TerminalId) -> Option<(PaneId, &str)> {
        let pane = self.workspaces.pane_showing(terminal)?;
        let restored = (self.workspaces.restored.iter()).find(|restored| restored.pane == pane)?;
        Some((pane, restored.ran.as_deref()?))
    }

    /// The offer the pane showing `terminal`, `cols` wide and `rows`
    /// high, shows, if any.
    pub(crate) fn ran_offer(&self, terminal: TerminalId, cols: u16, rows: u16) -> Option<Offer> {
        let (_, command) = self.ran_in(terminal)?;
        Offer::new(command, cols, rows)
    }

    /// The offer drawn on the pane showing `terminal`, in the tile it's
    /// in now, if one is.
    fn drawn_offer(&self, terminal: TerminalId) -> Option<(PaneId, Offer)> {
        let (pane, command) = self.ran_in(terminal)?;
        let placement = self.placement()?;
        let &(_, tile) = placement.panes.iter().find(|(id, _)| *id == pane)?;
        Some((pane, Offer::new(command, tile.cols, tile.rows)?))
    }

    /// Takes `press` for the offer drawn on the focused terminal, in
    /// normal mode, with no search being typed nor a command half typed,
    /// and pressed, not typed by a macro or a binding: `R` puts it at the
    /// prompt, and Esc dismisses it, then goes on to do what it does.
    /// Returns whether the key went no further.
    pub(super) fn ran_key(&mut self, press: &KeyPress<'_>) -> bool {
        if self.key_from_frame() || self.frames_typing() {
            return false;
        }
        let Some(terminal) = self.focused_terminal() else {
            return false;
        };
        let Some((pane, _)) = self.drawn_offer(terminal) else {
            return false;
        };
        let normal = (self.panes.get(&terminal)).is_some_and(|shown| {
            shown.mode == Mode::Normal && !shown.is_chat() && !shown.prompting() && !shown.pending()
        });
        if !normal || press.modifiers.ctrl || press.modifiers.alt {
            return false;
        }
        match press.key {
            Key::Character(text) if text == "R" => {
                self.put_at_prompt(pane, terminal);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.dismiss_ran(pane);
                false
            }
            _ => false,
        }
    }

    /// A left click on `cell` of `terminal`: on its offer, it puts the
    /// command at the prompt or dismisses it, and goes no further. Returns
    /// whether it was on the offer.
    pub(super) fn ran_click(&mut self, terminal: TerminalId, cell: [i64; 2]) -> bool {
        let Some((pane, offer)) = self.drawn_offer(terminal) else {
            return false;
        };
        if cell[0] != i64::from(offer.row) {
            return false;
        }
        let Ok(col) = u16::try_from(cell[1]) else {
            return false;
        };
        match offer.clicked(col) {
            None => return false,
            Some(Clicked::Put) => self.put_at_prompt(pane, terminal),
            Some(Clicked::Dismiss) => self.dismiss_ran(pane),
            Some(Clicked::Nothing) => {}
        }
        true
    }

    /// Types the command `pane`'s terminal was running at its shell's
    /// prompt, as if by the user, without Enter, and takes the offer away.
    fn put_at_prompt(&mut self, pane: PaneId, terminal: TerminalId) {
        let Some((_, command)) = self.ran_in(terminal) else {
            return;
        };
        // The daemon sends one line, with no control characters: but none
        // goes into a shell from here regardless. Pasted, as a shell that
        // asked takes it, bracketed, as text to edit, never keys.
        if !command.chars().any(char::is_control) {
            let command = command.to_owned();
            if let Some(shown) = self.panes.get_mut(&terminal) {
                shown.paste(&command);
            }
        }
        self.dismiss_ran(pane);
    }

    /// Takes away `pane`'s offer to run its command again, here at once
    /// and in the daemon, which tells every window.
    fn dismiss_ran(&mut self, pane: PaneId) {
        let restored = &mut self.workspaces.restored;
        if let Some(entry) = restored.iter_mut().find(|restored| restored.pane == pane) {
            entry.ran = None;
        }
        restored.retain(|restored| restored.ran.is_some() || restored.place.is_some());
        if self.connected && self.daemon_has(wire::RESTORED) {
            self.outbox.push(ClientMessage::Dismiss { pane });
        }
    }
}

impl Desk {
    pub(super) fn resume_shortcuts(&self, context: &mut crate::shortcuts::Context) {
        if self
            .focused_terminal()
            .and_then(|id| self.drawn_offer(id))
            .is_some()
        {
            context.add(&[
                ("R", "Put the previous command at the shell prompt"),
                ("Esc", "Dismiss the previous-command offer"),
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_sits_at_the_bottom_right_and_its_keys_are_where_theyre_drawn() {
        let offer = Offer::new("cargo watch", 80, 24).unwrap();
        assert_eq!(offer.row, 23);
        assert_eq!(
            offer.text,
            "was running: cargo watch · R puts it at the prompt · Esc dismiss"
        );
        assert_eq!(&offer.text[offer.command.clone()], "cargo watch");
        let width = offer.width();
        // A cell from the right edge.
        assert_eq!(offer.col + width, 79);
        let text_start = offer.col + 1;
        let put = u16::try_from("was running: cargo watch · ".chars().count()).unwrap();
        assert_eq!(offer.put, text_start + put..text_start + put + 23);
        assert_eq!(offer.clicked(offer.put.start), Some(Clicked::Put));
        assert_eq!(offer.clicked(offer.dismiss.end - 1), Some(Clicked::Dismiss));
        assert_eq!(offer.clicked(offer.col), Some(Clicked::Nothing));
        assert_eq!(offer.clicked(offer.col - 1), None);
        assert_eq!(offer.clicked(79), None);
    }

    #[test]
    fn a_long_command_is_cut_short_and_a_narrow_pane_shows_none() {
        let long = format!("cargo test {}", "x".repeat(200));
        let offer = Offer::new(&long, 80, 24).unwrap();
        assert!(offer.text.contains("cargo test xx"));
        assert!(offer.text[offer.command.clone()].ends_with('…'));
        assert_eq!(offer.col + offer.width(), 79);
        // Narrower, the keys are said shortly.
        let offer = Offer::new(&long, 40, 24).unwrap();
        assert!(
            offer.text.ends_with("cargo test xx… · R · Esc"),
            "{}",
            offer.text
        );
        assert_eq!(offer.clicked(offer.put.start), Some(Clicked::Put));
        assert!(Offer::new(&long, 30, 24).is_none());
        assert!(Offer::new("ls", 20, 24).is_none());
        assert!(Offer::new("ls", 80, 0).is_none());
    }
}
