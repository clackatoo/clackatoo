//! Turns parsed escape sequences into emulator operations.
//!
//! The parser splits the byte stream into text, control characters and
//! escape sequences (ESC, CSI, OSC, DCS). This module decides
//! what each one means. The supported set is what shells, ncurses apps (htop)
//! and vim use with `TERM=xterm-256color`. Anything else is logged at trace
//! level and ignored, the same way xterm ignores what it doesn't know.

use crate::charset::{Charset, Slot};
use crate::parser::{Params, Perform};
use crate::sgr;
use crate::terminal::Emulator;

/// The reply to DA1 (primary device attributes): a VT220 (62) with ANSI
/// colour (22). fish 4 sends this query at startup and waits for the answer,
/// so it must always be sent.
const PRIMARY_DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;22c";

impl Perform for Emulator {
    fn print(&mut self, text: &str) {
        self.print_text(text);
    }

    fn print_ascii(&mut self, text: &[u8]) {
        Emulator::print_ascii(self, text);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // BEL. A bell will reach clients once they can show one.
            0x07 => {}
            0x08 => self.backspace(),
            0x09 => self.tab_forward(1),
            // LF, and VT and FF, which terminals treat as LF.
            0x0A..=0x0C => self.line_feed(),
            0x0D => self.carriage_return(),
            // SO and SI: shift out to G1, shift back in to G0.
            0x0E => self.cursor.charsets.shift(Slot::G1),
            0x0F => self.cursor.charsets.shift(Slot::G0),
            _ => log::trace!("unhandled control character {byte:#04x}"),
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            log::trace!("ignoring CSI {action} with too many parameters or intermediates");
            return;
        }
        let arg = |i: usize| params.iter().nth(i).and_then(|p| p.first().copied());
        // Movement counts treat a missing or zero parameter as 1.
        let count = |i: usize| arg(i).filter(|&n| n != 0).map_or(1, usize::from);
        let position = |i: usize| count(i) - 1;

        match (intermediates, action) {
            ([], '@') => self.insert_chars(count(0)),
            ([], 'A') => self.move_up(count(0)),
            ([], 'B' | 'e') => self.move_down(count(0)),
            ([], 'C' | 'a') => self.move_right(count(0)),
            ([], 'D') => self.move_left(count(0)),
            ([], 'E') => {
                self.move_down(count(0));
                self.carriage_return();
            }
            ([], 'F') => {
                self.move_up(count(0));
                self.carriage_return();
            }
            ([], 'G' | '`') => self.set_col(position(0)),
            ([], 'H' | 'f') => self.move_to_origin(position(0), position(1)),
            ([], 'I') => self.tab_forward(count(0)),
            ([], 'J') => self.erase_in_display(arg(0).unwrap_or(0)),
            ([], 'K') => self.erase_in_line(arg(0).unwrap_or(0)),
            ([], 'L') => self.insert_lines(count(0)),
            ([], 'M') => self.delete_lines(count(0)),
            ([], 'P') => self.delete_chars(count(0)),
            ([], 'S') => self.scroll_up(count(0)),
            // With more parameters, `CSI … T` is xterm's mouse highlight
            // tracking, which nothing uses.
            ([], 'T') if params.len() <= 1 => self.scroll_down(count(0)),
            ([], 'X') => self.erase_chars(count(0)),
            ([], 'Z') => self.tab_backward(count(0)),
            ([], 'b') => self.repeat_last(count(0)),
            ([], 'c') if arg(0).unwrap_or(0) == 0 => self.reply(PRIMARY_DEVICE_ATTRIBUTES),
            ([], 'd') => self.set_line_origin(position(0)),
            ([], 'g') => match arg(0).unwrap_or(0) {
                0 => self.clear_tab_stop(),
                3 => self.clear_all_tab_stops(),
                mode => log::trace!("unhandled TBC mode {mode}"),
            },
            ([], 'h' | 'l') => {
                for param in params {
                    self.set_ansi_mode(first(param), action == 'h');
                }
            }
            ([], 'm') => sgr::apply(&mut self.cursor.pen, params),
            ([], 'n') => self.device_status_report(arg(0).unwrap_or(0), false),
            ([], 'r') => {
                let edge = |i: usize| arg(i).filter(|&n| n != 0).map(usize::from);
                self.set_scroll_region(edge(0), edge(1));
            }
            // SCOSC and SCORC: the SCO spellings of save and restore cursor.
            // With parameters, `CSI s` sets left and right margins instead.
            ([], 's') if no_params(params) => self.save_cursor(),
            ([], 'u') if no_params(params) => self.restore_cursor(),
            ([b'?'], 'h' | 'l') => {
                for param in params {
                    self.set_dec_mode(first(param), action == 'h');
                }
            }
            ([b'?'], 'n') => self.device_status_report(arg(0).unwrap_or(0), true),
            ([b'>'], 'q') if arg(0).unwrap_or(0) == 0 => self.report_version(),
            ([b'!'], 'p') => self.soft_reset(),
            _ => log::trace!("unhandled CSI {intermediates:?} {params:?} {action}"),
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            log::trace!("ignoring ESC {byte:#04x} with too many intermediates");
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.index();
                self.carriage_return();
            }
            ([], b'H') => self.set_tab_stop(),
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.full_reset(),
            ([], b'=') => self.set_app_keypad(true),
            ([], b'>') => self.set_app_keypad(false),
            ([b'#'], b'8') => self.screen_alignment_test(),
            ([slot @ (b'(' | b')')], designator) => {
                let slot = if *slot == b'(' { Slot::G0 } else { Slot::G1 };
                match Charset::from_designator(designator) {
                    Some(charset) => self.cursor.charsets.designate(slot, charset),
                    None => log::trace!("unhandled charset {designator:#04x}"),
                }
            }
            _ => log::trace!("unhandled ESC {intermediates:?} {byte:#04x}"),
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params {
            // OSC 0 sets the icon name and title, OSC 2 just the title. With
            // no icons to name, both set the title.
            [b"0" | b"2", title @ ..] => {
                self.title = String::from_utf8_lossy(&title.join(&b';')).into_owned();
            }
            _ => log::trace!(
                "unhandled OSC {:?}",
                params.first().map(|p| String::from_utf8_lossy(p))
            ),
        }
    }
}

/// Whether a sequence came without parameters. The parser reports `CSI s` as having
/// one parameter of 0.
fn no_params(params: &Params) -> bool {
    params.len() <= 1 && params.iter().flatten().all(|&value| value == 0)
}

/// A parameter's main value, ignoring subparameters.
fn first(param: &[u16]) -> u16 {
    param.first().copied().unwrap_or(0)
}

impl Emulator {
    /// DSR (device status report): 5 asks "are you OK?", 6 asks where the
    /// cursor is. `private` is the DEC form (`CSI ? 6 n`), which marks its
    /// reply with a `?`.
    fn device_status_report(&mut self, query: u16, private: bool) {
        match query {
            5 if !private => self.reply(b"\x1b[0n"),
            6 => {
                let (line, col) = self.reported_position();
                let marker = if private { "?" } else { "" };
                let reply = format!("\x1b[{marker}{line};{col}R");
                self.reply(reply.as_bytes());
            }
            _ => log::trace!("unhandled DSR {query}"),
        }
    }

    /// XTVERSION: reports the terminal's name and version.
    fn report_version(&mut self) {
        let reply = format!("\x1bP>|clackatoo {}\x1b\\", env!("CARGO_PKG_VERSION"));
        self.reply(reply.as_bytes());
    }
}
