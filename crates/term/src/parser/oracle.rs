//! The parser's oracle: `vte`, which has years of use in Alacritty behind
//! it. For any input, this parser must report what vte reports, apart from
//! the differences the module documentation lists. The tests check it with
//! proptest, and the fuzzer with what it finds (see `crate::fuzzing`).

use super::{Params, Parser, Perform};

/// Everything a parser reports, in a form two parsers can be compared in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Print(String),
    Execute(u8),
    Csi {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        ignore: bool,
        action: char,
    },
    Esc {
        intermediates: Vec<u8>,
        ignore: bool,
        byte: u8,
    },
    Osc {
        params: Vec<Vec<u8>>,
        bell_terminated: bool,
    },
    Hook {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        ignore: bool,
        action: char,
    },
    Put(Vec<u8>),
    Unhook,
}

/// Records events, joining consecutive text and DCS data, which either
/// parser may split differently.
#[derive(Default)]
struct Recorder(Vec<Event>);

impl Recorder {
    fn print(&mut self, text: &str) {
        if let Some(Event::Print(last)) = self.0.last_mut() {
            last.push_str(text);
        } else if !text.is_empty() {
            self.0.push(Event::Print(text.to_owned()));
        }
    }

    fn put(&mut self, bytes: &[u8]) {
        if let Some(Event::Put(last)) = self.0.last_mut() {
            last.extend_from_slice(bytes);
        } else {
            self.0.push(Event::Put(bytes.to_vec()));
        }
    }
}

impl Perform for Recorder {
    fn print(&mut self, text: &str) {
        assert!(
            !text.chars().any(char::is_control),
            "text {text:?} holds a control character"
        );
        Recorder::print(self, text);
    }

    fn print_ascii(&mut self, text: &[u8]) {
        assert!(
            text.iter().all(|byte| (0x20..0x7F).contains(byte)),
            "{text:?} is not printable ASCII"
        );
        Recorder::print(self, std::str::from_utf8(text).expect("ASCII"));
    }

    fn execute(&mut self, byte: u8) {
        self.0.push(Event::Execute(byte));
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.0.push(Event::Csi {
            params: params.iter().map(<[u16]>::to_vec).collect(),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.0.push(Event::Esc {
            intermediates: intermediates.to_vec(),
            ignore,
            byte,
        });
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.0.push(Event::Osc {
            params: params.iter().map(|part| part.to_vec()).collect(),
            bell_terminated,
        });
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.0.push(Event::Hook {
            params: params.iter().map(<[u16]>::to_vec).collect(),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }

    fn put(&mut self, bytes: &[u8]) {
        Recorder::put(self, bytes);
    }

    fn unhook(&mut self) {
        self.0.push(Event::Unhook);
    }
}

impl vte::Perform for Recorder {
    fn print(&mut self, ch: char) {
        // We drop DEL on purpose; vte prints it.
        if ch != '\x7f' {
            Recorder::print(self, ch.encode_utf8(&mut [0; 4]));
        }
    }

    fn execute(&mut self, byte: u8) {
        self.0.push(Event::Execute(byte));
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        self.0.push(Event::Csi {
            params: params.iter().map(<[u16]>::to_vec).collect(),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.0.push(Event::Esc {
            intermediates: intermediates.to_vec(),
            ignore,
            byte,
        });
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.0.push(Event::Osc {
            params: params.iter().map(|part| part.to_vec()).collect(),
            bell_terminated,
        });
    }

    fn hook(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, action: char) {
        self.0.push(Event::Hook {
            params: params.iter().map(<[u16]>::to_vec).collect(),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }

    fn put(&mut self, byte: u8) {
        Recorder::put(self, &[byte]);
    }

    fn unhook(&mut self) {
        self.0.push(Event::Unhook);
    }
}

pub(crate) fn ours(pieces: &[&[u8]]) -> Vec<Event> {
    let mut parser = Parser::new();
    let mut recorder = Recorder::default();
    for piece in pieces {
        parser.advance(&mut recorder, piece);
    }
    recorder.0
}

pub(crate) fn vtes(bytes: &[u8]) -> Vec<Event> {
    let mut parser = vte::Parser::new();
    let mut recorder = Recorder::default();
    parser.advance(&mut recorder, bytes);
    recorder.0
}

/// Splits `bytes` at `cuts`, which are positions in `0..=bytes.len()`, in
/// any order.
pub(crate) fn split(bytes: &[u8], mut cuts: Vec<usize>) -> Vec<&[u8]> {
    cuts.sort_unstable();
    let mut pieces = Vec::new();
    let mut start = 0;
    for cut in cuts.into_iter().chain([bytes.len()]) {
        pieces.push(&bytes[start..cut]);
        start = cut;
    }
    pieces
}
