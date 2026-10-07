//! Splitting a byte stream into text, control characters and escape
//! sequences.
//!
//! This is Paul Williams' DEC-compatible parser (<https://vt100.net/emu/dec_ansi_parser>)
//! with the same additions and rules as the `vte` crate, which clackatoo
//! used before: UTF-8 text, OSC strings that BEL can end, 7-bit sequences
//! only, and vte's treatment of C1 controls and invalid UTF-8. Matching vte
//! exactly lets vte check this parser: the differential test in `tests.rs`
//! feeds both the same bytes and requires the same events.
//!
//! Where it differs from vte, it does so on purpose:
//! - Text arrives in runs, borrowed straight from the input, rather than a
//!   character at a time. That is most of the point.
//! - A UTF-8 character split between two calls to [`Parser::advance`] is
//!   always put back together; vte can drop characters there.
//! - DEL (0x7F) between sequences is dropped. vte passes it on as text,
//!   which terminals then ignore.
//! - OSC strings are capped at [`MAX_OSC_LEN`] bytes, where vte keeps
//!   growing its buffer.
//!
//! It knows nothing of what sequences mean; [`Perform`] implementations
//! decide that.

mod params;
mod scan;
#[cfg(test)]
mod tests;

pub(crate) use params::Params;

/// The most intermediate bytes (0x20–0x2F, like the `$` in `CSI 1 $ p`) a
/// sequence keeps. More flag it as ignored.
const MAX_INTERMEDIATES: usize = 2;

/// The most `;`-separated parts of an OSC string that are split out. The
/// rest stay joined to the last part.
const MAX_OSC_PARAMS: usize = 16;

/// The most bytes of one OSC string that are kept; the rest are dropped.
/// Generous, so that OSC 52 clipboard writes of large selections still fit,
/// but bounded, so a stream that never ends its string can't use unbounded
/// memory.
pub(crate) const MAX_OSC_LEN: usize = 1 << 20;

/// U+FFFD REPLACEMENT CHARACTER, shown for invalid UTF-8.
const REPLACEMENT: &str = "\u{FFFD}";

/// What the parser found, as it finds it.
pub(crate) trait Perform {
    /// Printable text. It never contains a C0 control, DEL or a C1 control
    /// (U+0080–U+009F). A run of text can arrive in several pieces, through
    /// this and [`print_ascii`](Self::print_ascii).
    fn print(&mut self, text: &str);

    /// Printable ASCII text (0x20–0x7E) only: the common case, passed as
    /// bytes so nobody has to check or decode them.
    fn print_ascii(&mut self, text: &[u8]);

    /// A C0 or C1 control character to act on, such as `\n`.
    fn execute(&mut self, byte: u8);

    /// A finished CSI sequence. `ignore` is set if it had too many parameters
    /// or intermediates to keep them all.
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char);

    /// A finished escape sequence other than CSI, OSC and DCS.
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);

    /// A finished OSC string, split at `;`.
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool);

    /// The start of a DCS string. Its data follows through
    /// [`put`](Self::put), and [`unhook`](Self::unhook) ends it.
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}

    /// Some data of the current DCS string.
    fn put(&mut self, _bytes: &[u8]) {}

    /// The end of the current DCS string.
    fn unhook(&mut self) {}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmApcString,
}

/// The parser. Feed it bytes with [`advance`](Self::advance) as they arrive;
/// splitting the stream differently never changes what it finds.
#[derive(Clone, Debug, Default)]
pub(crate) struct Parser {
    state: State,
    params: Params,
    /// The parameter being collected.
    param: u16,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_len: usize,
    /// Set when a sequence overflows its parameters or intermediates.
    ignoring: bool,
    osc: Vec<u8>,
    /// Where each split-out OSC part ends in `osc`.
    osc_ends: [usize; MAX_OSC_PARAMS],
    osc_parts: usize,
    /// The start of a UTF-8 character that the last call ended partway
    /// through.
    partial: [u8; 4],
    partial_len: usize,
}

impl Parser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Parses `bytes`, reporting what it finds to `performer`.
    pub(crate) fn advance(&mut self, performer: &mut impl Perform, bytes: &[u8]) {
        let mut i = 0;
        if self.partial_len > 0 {
            i = self.finish_partial(performer, bytes);
        }
        while i < bytes.len() {
            i += match self.state {
                State::Ground => self.advance_ground(performer, &bytes[i..]),
                State::OscString => self.advance_osc_string(performer, &bytes[i..]),
                State::DcsPassthrough => self.advance_dcs_passthrough(performer, &bytes[i..]),
                State::DcsIgnore | State::SosPmApcString => {
                    self.skip_string(performer, &bytes[i..])
                }
                _ => self.advance_sequence(performer, &bytes[i..]),
            };
        }
    }

    // --- Text ---------------------------------------------------------------

    /// Handles bytes between sequences until the next escape sequence starts
    /// or the input ends. Returns how many it used.
    fn advance_ground(&mut self, performer: &mut impl Perform, bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() {
            let ascii = scan::printable_ascii_len(&bytes[i..]);
            if ascii > 0 {
                performer.print_ascii(&bytes[i..i + ascii]);
                i += ascii;
            }
            let Some(&byte) = bytes.get(i) else {
                break;
            };
            match byte {
                0x1B => {
                    self.enter(State::Escape);
                    return i + 1;
                }
                0x7F => i += 1,
                0x00..=0x1F => {
                    performer.execute(byte);
                    i += 1;
                }
                _ => {
                    i += self.advance_utf8(performer, &bytes[i..]);
                    if self.partial_len > 0 {
                        break;
                    }
                }
            }
        }
        i
    }

    /// Handles text starting with a non-ASCII byte, up to the next control
    /// character. Returns how many bytes it used. An unfinished character at
    /// the very end of the input is kept for the next call.
    fn advance_utf8(&mut self, performer: &mut impl Perform, bytes: &[u8]) -> usize {
        let end = scan::text_len(bytes);
        let text = &bytes[..end];
        match std::str::from_utf8(text) {
            Ok(text) => {
                print_text(performer, text);
                end
            }
            Err(err) => {
                let valid = err.valid_up_to();
                let valid_text = std::str::from_utf8(&text[..valid]).expect("checked valid");
                print_text(performer, valid_text);
                match err.error_len() {
                    Some(len) => {
                        invalid_utf8(performer, text[valid], len);
                        valid + len
                    }
                    // Unfinished, and the input ends here: wait for the rest.
                    None if end == bytes.len() => {
                        let unfinished = &text[valid..];
                        self.partial[..unfinished.len()].copy_from_slice(unfinished);
                        self.partial_len = unfinished.len();
                        end
                    }
                    // Unfinished, but a control character cuts it off.
                    None => {
                        performer.print(REPLACEMENT);
                        end
                    }
                }
            }
        }
    }

    /// Completes a character the last call ended partway through, from the
    /// start of `bytes`. Returns how many bytes of `bytes` it used.
    fn finish_partial(&mut self, performer: &mut impl Perform, bytes: &[u8]) -> usize {
        let held = self.partial_len;
        let mut buffer = [0; 4];
        buffer[..held].copy_from_slice(&self.partial[..held]);
        let added = bytes.len().min(4 - held);
        buffer[held..held + added].copy_from_slice(&bytes[..added]);
        let candidate = &buffer[..held + added];

        match std::str::from_utf8(candidate) {
            // The held bytes are a valid start, so if anything is valid, it is
            // the finished character.
            Ok(_) => self.finish_char(performer, candidate, held),
            Err(err) if err.valid_up_to() > 0 => {
                self.finish_char(performer, &candidate[..err.valid_up_to()], held)
            }
            Err(err) => {
                if let Some(len) = err.error_len() {
                    invalid_utf8(performer, candidate[0], len);
                    self.partial_len = 0;
                    // The invalid bytes include all those held, never fewer.
                    len - held
                } else {
                    // Still unfinished: hold everything there was.
                    self.partial[held..held + added].copy_from_slice(&bytes[..added]);
                    self.partial_len = held + added;
                    added
                }
            }
        }
    }

    /// Reports the first character of `valid`, which starts with the `held`
    /// bytes. Returns how many bytes beyond those it took.
    fn finish_char(&mut self, performer: &mut impl Perform, valid: &[u8], held: usize) -> usize {
        let text = std::str::from_utf8(valid).expect("checked valid");
        let ch = text.chars().next().expect("held bytes start a character");
        let len = ch.len_utf8();
        print_text(performer, &text[..len]);
        self.partial_len = 0;
        len - held
    }

    // --- Sequences ----------------------------------------------------------

    /// Handles bytes one at a time while inside an escape, CSI or DCS
    /// header, until the state changes to one handled elsewhere. Returns how
    /// many bytes it used.
    fn advance_sequence(&mut self, performer: &mut impl Perform, bytes: &[u8]) -> usize {
        let mut i = 0;
        while let Some(&byte) = bytes.get(i) {
            i += 1;
            if self.state == State::CsiParam && byte.is_ascii_digit() {
                // The common case inside a CSI: collect a whole number.
                self.param_digit(byte);
                while let Some(&digit) = bytes.get(i).filter(|byte| byte.is_ascii_digit()) {
                    self.param_digit(digit);
                    i += 1;
                }
                continue;
            }
            self.step(performer, byte);
            if !matches!(
                self.state,
                State::Escape
                    | State::EscapeIntermediate
                    | State::CsiEntry
                    | State::CsiParam
                    | State::CsiIntermediate
                    | State::CsiIgnore
                    | State::DcsEntry
                    | State::DcsParam
                    | State::DcsIntermediate
            ) {
                break;
            }
        }
        i
    }

    /// One byte of an escape, CSI or DCS header.
    fn step(&mut self, performer: &mut impl Perform, byte: u8) {
        match self.state {
            State::Escape => self.step_escape(performer, byte),
            State::EscapeIntermediate => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
                0x20..=0x2F => self.collect(byte),
                0x30..=0x7E => self.esc_dispatch(performer, byte),
                0x7F => {}
                _ => self.anywhere(performer, byte),
            },
            State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore => {
                self.step_csi(performer, byte);
            }
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => {
                self.step_dcs_header(performer, byte);
            }
            State::Ground
            | State::DcsPassthrough
            | State::DcsIgnore
            | State::OscString
            | State::SosPmApcString => unreachable!("{:?} is handled in bulk", self.state),
        }
    }

    fn step_csi(&mut self, performer: &mut impl Perform, byte: u8) {
        match self.state {
            State::CsiEntry => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
                0x20..=0x2F => {
                    self.collect(byte);
                    self.state = State::CsiIntermediate;
                }
                0x30..=0x39 => {
                    self.param_digit(byte);
                    self.state = State::CsiParam;
                }
                0x3A => {
                    self.subparam();
                    self.state = State::CsiParam;
                }
                0x3B => {
                    self.next_param();
                    self.state = State::CsiParam;
                }
                0x3C..=0x3F => {
                    self.collect(byte);
                    self.state = State::CsiParam;
                }
                0x40..=0x7E => self.csi_dispatch(performer, byte),
                _ => self.anywhere(performer, byte),
            },
            State::CsiParam => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
                0x20..=0x2F => {
                    self.collect(byte);
                    self.state = State::CsiIntermediate;
                }
                0x30..=0x39 => self.param_digit(byte),
                0x3A => self.subparam(),
                0x3B => self.next_param(),
                0x3C..=0x3F => self.state = State::CsiIgnore,
                0x40..=0x7E => self.csi_dispatch(performer, byte),
                0x7F => {}
                _ => self.anywhere(performer, byte),
            },
            State::CsiIntermediate => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
                0x20..=0x2F => self.collect(byte),
                0x30..=0x3F => self.state = State::CsiIgnore,
                0x40..=0x7E => self.csi_dispatch(performer, byte),
                _ => self.anywhere(performer, byte),
            },
            State::CsiIgnore => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
                0x20..=0x3F | 0x7F => {}
                0x40..=0x7E => self.state = State::Ground,
                _ => self.anywhere(performer, byte),
            },
            _ => unreachable!("not in a CSI sequence"),
        }
    }

    fn step_dcs_header(&mut self, performer: &mut impl Perform, byte: u8) {
        match self.state {
            State::DcsEntry => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
                0x20..=0x2F => {
                    self.collect(byte);
                    self.state = State::DcsIntermediate;
                }
                0x30..=0x39 => {
                    self.param_digit(byte);
                    self.state = State::DcsParam;
                }
                0x3A => {
                    self.subparam();
                    self.state = State::DcsParam;
                }
                0x3B => {
                    self.next_param();
                    self.state = State::DcsParam;
                }
                0x3C..=0x3F => {
                    self.collect(byte);
                    self.state = State::DcsParam;
                }
                0x40..=0x7E => self.hook(performer, byte),
                _ => self.anywhere(performer, byte),
            },
            State::DcsParam => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
                0x20..=0x2F => {
                    self.collect(byte);
                    self.state = State::DcsIntermediate;
                }
                0x30..=0x39 => self.param_digit(byte),
                0x3A => self.subparam(),
                0x3B => self.next_param(),
                0x3C..=0x3F => self.state = State::DcsIgnore,
                0x40..=0x7E => self.hook(performer, byte),
                _ => self.anywhere(performer, byte),
            },
            State::DcsIntermediate => match byte {
                0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x7F => {}
                0x20..=0x2F => self.collect(byte),
                0x30..=0x3F => self.state = State::DcsIgnore,
                0x40..=0x7E => self.hook(performer, byte),
                _ => self.anywhere(performer, byte),
            },
            _ => unreachable!("not in a DCS header"),
        }
    }

    fn step_escape(&mut self, performer: &mut impl Perform, byte: u8) {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => performer.execute(byte),
            0x18 | 0x1A => {
                performer.execute(byte);
                self.state = State::Ground;
            }
            0x20..=0x2F => {
                self.collect(byte);
                self.state = State::EscapeIntermediate;
            }
            b'P' => self.enter(State::DcsEntry),
            b'[' => self.enter(State::CsiEntry),
            b']' => {
                self.osc.clear();
                self.osc_parts = 0;
                self.state = State::OscString;
            }
            // SOS, PM and APC: strings nothing uses, skipped whole.
            b'X' | b'^' | b'_' => self.state = State::SosPmApcString,
            0x30..=0x7E => self.esc_dispatch(performer, byte),
            // A second ESC, DEL and 8-bit bytes change nothing.
            _ => {}
        }
    }

    /// What most states do with CAN, SUB and ESC; other bytes are ignored.
    fn anywhere(&mut self, performer: &mut impl Perform, byte: u8) {
        match byte {
            0x18 | 0x1A => {
                performer.execute(byte);
                self.state = State::Ground;
            }
            0x1B => self.enter(State::Escape),
            _ => {}
        }
    }

    /// Starts a new sequence in `state`, forgetting the last one's
    /// parameters.
    fn enter(&mut self, state: State) {
        self.intermediate_len = 0;
        self.ignoring = false;
        self.param = 0;
        self.params.clear();
        self.state = state;
    }

    fn collect(&mut self, byte: u8) {
        if self.intermediate_len == MAX_INTERMEDIATES {
            self.ignoring = true;
        } else {
            self.intermediates[self.intermediate_len] = byte;
            self.intermediate_len += 1;
        }
    }

    fn param_digit(&mut self, byte: u8) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.param = self
                .param
                .saturating_mul(10)
                .saturating_add(u16::from(byte - b'0'));
        }
    }

    fn next_param(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
            self.param = 0;
        }
    }

    fn subparam(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.extend(self.param);
            self.param = 0;
        }
    }

    /// Ends the parameters with the one being collected.
    fn finish_params(&mut self) {
        if self.params.is_full() {
            self.ignoring = true;
        } else {
            self.params.push(self.param);
        }
    }

    fn csi_dispatch(&mut self, performer: &mut impl Perform, byte: u8) {
        self.finish_params();
        let intermediates = &self.intermediates[..self.intermediate_len];
        performer.csi_dispatch(&self.params, intermediates, self.ignoring, char::from(byte));
        self.state = State::Ground;
    }

    fn esc_dispatch(&mut self, performer: &mut impl Perform, byte: u8) {
        let intermediates = &self.intermediates[..self.intermediate_len];
        performer.esc_dispatch(intermediates, self.ignoring, byte);
        self.state = State::Ground;
    }

    fn hook(&mut self, performer: &mut impl Perform, byte: u8) {
        self.finish_params();
        let intermediates = &self.intermediates[..self.intermediate_len];
        performer.hook(&self.params, intermediates, self.ignoring, char::from(byte));
        self.state = State::DcsPassthrough;
    }

    // --- Strings ------------------------------------------------------------

    /// Collects an OSC string until it ends or the input does. Returns how
    /// many bytes it used.
    fn advance_osc_string(&mut self, performer: &mut impl Perform, bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() {
            let text = bytes[i..]
                .iter()
                .position(|&byte| byte < 0x20 || byte == b';')
                .unwrap_or(bytes.len() - i);
            self.osc_put(&bytes[i..i + text]);
            i += text;
            let Some(&byte) = bytes.get(i) else {
                break;
            };
            i += 1;
            match byte {
                b';' => self.osc_split(),
                0x07 => {
                    self.osc_end(performer, true);
                    self.state = State::Ground;
                    break;
                }
                0x18 | 0x1A => {
                    self.osc_end(performer, false);
                    performer.execute(byte);
                    self.state = State::Ground;
                    break;
                }
                0x1B => {
                    self.osc_end(performer, false);
                    self.enter(State::Escape);
                    break;
                }
                // Other C0 controls are ignored inside the string.
                _ => {}
            }
        }
        i
    }

    fn osc_put(&mut self, bytes: &[u8]) {
        let room = MAX_OSC_LEN - self.osc.len();
        self.osc.extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    /// Ends one `;`-separated part of the OSC string. After the last part
    /// that is split out, separators stay in the text.
    fn osc_split(&mut self) {
        if self.osc_parts == MAX_OSC_PARAMS {
            return;
        }
        self.osc_ends[self.osc_parts] = self.osc.len();
        self.osc_parts += 1;
    }

    fn osc_end(&mut self, performer: &mut impl Perform, bell_terminated: bool) {
        self.osc_split();
        let mut parts: [&[u8]; MAX_OSC_PARAMS] = [&[]; MAX_OSC_PARAMS];
        let mut start = 0;
        for (part, &end) in parts.iter_mut().zip(&self.osc_ends[..self.osc_parts]) {
            *part = &self.osc[start..end];
            start = end;
        }
        performer.osc_dispatch(&parts[..self.osc_parts], bell_terminated);
        self.osc.clear();
        self.osc_parts = 0;
    }

    /// Passes DCS data on until the string ends or the input does. Returns
    /// how many bytes it used.
    fn advance_dcs_passthrough(&mut self, performer: &mut impl Perform, bytes: &[u8]) -> usize {
        let mut i = 0;
        while i < bytes.len() {
            let data = bytes[i..]
                .iter()
                .position(|&byte| matches!(byte, 0x18 | 0x1A | 0x1B | 0x7F..))
                .unwrap_or(bytes.len() - i);
            if data > 0 {
                performer.put(&bytes[i..i + data]);
                i += data;
            }
            let Some(&byte) = bytes.get(i) else {
                break;
            };
            i += 1;
            match byte {
                0x18 | 0x1A => {
                    performer.unhook();
                    performer.execute(byte);
                    self.state = State::Ground;
                    break;
                }
                0x1B => {
                    performer.unhook();
                    self.enter(State::Escape);
                    break;
                }
                // ST as a single 8-bit byte.
                0x9C => {
                    performer.unhook();
                    self.state = State::Ground;
                    break;
                }
                // DEL and other 8-bit bytes are dropped.
                _ => {}
            }
        }
        i
    }

    /// Skips a string nothing uses until CAN, SUB or ESC ends it. Returns how
    /// many bytes it used.
    fn skip_string(&mut self, performer: &mut impl Perform, bytes: &[u8]) -> usize {
        match bytes
            .iter()
            .position(|&byte| matches!(byte, 0x18 | 0x1A | 0x1B))
        {
            Some(end) => {
                self.anywhere(performer, bytes[end]);
                end + 1
            }
            None => bytes.len(),
        }
    }
}

/// Reports `text`, which may hold C1 controls (U+0080–U+009F, encoded as
/// `C2 80`–`C2 9F`). Those are executed, as vte does, and the rest printed.
fn print_text(performer: &mut impl Perform, mut text: &str) {
    while let Some(c1) = find_c1(text.as_bytes()) {
        if c1 > 0 {
            performer.print(&text[..c1]);
        }
        performer.execute(text.as_bytes()[c1 + 1]);
        text = &text[c1 + 2..];
    }
    if !text.is_empty() {
        performer.print(text);
    }
}

/// Where the first C1 control in valid UTF-8 starts.
fn find_c1(bytes: &[u8]) -> Option<usize> {
    let mut from = 0;
    while let Some(lead) = bytes[from..].iter().position(|&byte| byte == 0xC2) {
        let at = from + lead;
        if (0x80..=0x9F).contains(&bytes[at + 1]) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Reports `len` invalid bytes starting with `first`, the way vte does: a
/// lone byte in 0x80–0x9F is a C1 control, anything else one replacement
/// character.
fn invalid_utf8(performer: &mut impl Perform, first: u8, len: usize) {
    if len == 1 && first <= 0x9F {
        performer.execute(first);
    } else {
        performer.print(REPLACEMENT);
    }
}
