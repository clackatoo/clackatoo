//! The parser's tests, chiefly a differential test against `vte`.
//!
//! vte has years of use in Alacritty behind it, so it serves as the oracle:
//! for any input, this parser must report what vte reports, apart from the
//! differences the module documentation lists. Ours is also fed the input in
//! random pieces, which must never change the result. (vte gets it whole,
//! because it can drop characters split between pieces.)

use proptest::prelude::*;

use super::MAX_OSC_LEN;
use super::oracle::{self, Event, ours, vtes};

/// Splits `bytes` at `cuts`.
fn split<'a>(bytes: &'a [u8], cuts: &[prop::sample::Index]) -> Vec<&'a [u8]> {
    oracle::split(
        bytes,
        (cuts.iter())
            .map(|cut| cut.index(bytes.len() + 1))
            .collect(),
    )
}

/// Bytes that are likely to make interesting sequences: the escape-sequence
/// alphabet, UTF-8 pieces both valid and not, and anything at all.
fn stream() -> impl Strategy<Value = Vec<u8>> {
    let fragment = prop_oneof![
        3 => prop::sample::select(vec![
            b"\x1b".to_vec(), b"\x1b[".to_vec(), b"\x1b]".to_vec(), b"\x1bP".to_vec(),
            b"\x1b\\".to_vec(), b"\x1bX".to_vec(), b"\x1b^".to_vec(), b"\x1b_".to_vec(),
            b"\x07".to_vec(), b"\x18".to_vec(), b"\x1a".to_vec(), b"\x9c".to_vec(),
            b";".to_vec(), b":".to_vec(), b"?".to_vec(), b"$".to_vec(), b" ".to_vec(),
            b"\x7f".to_vec(), b"\n".to_vec(), b"m".to_vec(), b"q".to_vec(),
        ]),
        3 => "[0-9]{1,6}".prop_map(String::into_bytes),
        2 => "[a-zA-Z ]{1,10}".prop_map(String::into_bytes),
        2 => prop::sample::select(vec![
            "é".as_bytes().to_vec(), "字".as_bytes().to_vec(), "😀".as_bytes().to_vec(),
            "\u{85}".as_bytes().to_vec(), "\u{9b}".as_bytes().to_vec(),
            b"\xcc".to_vec(), b"\xe5\xad".to_vec(), b"\xf0\x9f".to_vec(), b"\x80".to_vec(),
            b"\xc0\xaf".to_vec(), b"\xff".to_vec(), b"\xed\xa0\x80".to_vec(),
        ]),
        2 => prop::collection::vec(any::<u8>(), 1..8),
        4 => well_formed(),
    ];
    prop::collection::vec(fragment, 0..40).prop_map(|fragments| fragments.concat())
}

/// At least 2000 cases, or more if `PROPTEST_CASES` asks for them.
fn config() -> ProptestConfig {
    let default = ProptestConfig::default();
    ProptestConfig {
        cases: default.cases.max(2000),
        ..default
    }
}

/// Complete, ordinary sequences, which random bytes rarely spell out: the
/// shapes real programs send, and the ones the fast paths handle.
fn well_formed() -> impl Strategy<Value = Vec<u8>> {
    let number = prop_oneof![Just(String::new()), "[0-9]{1,3}", "[0-9]{4,7}"];
    let separator = prop::sample::select(vec![";", ";", ":"]);
    let params = prop::collection::vec((number, separator), 0..40).prop_map(|parts| {
        let mut text = String::new();
        for (number, separator) in &parts {
            text.push_str(number);
            text.push_str(separator);
        }
        text.pop();
        text
    });
    let marker = prop::sample::select(vec!["", "", "?", ">", "<", "="]);
    let intermediate = prop::sample::select(vec!["", "", "", " ", "$", "!"]);
    let csi = (marker, params.clone(), intermediate, 0x40u8..=0x7E).prop_map(
        |(marker, params, intermediate, end)| {
            format!("\x1b[{marker}{params}{intermediate}{}", char::from(end)).into_bytes()
        },
    );
    let osc = ("[0-9]{1,3}", "[ -:<-~]{0,20}", any::<bool>()).prop_map(|(n, text, bell)| {
        let end = if bell { "\x07" } else { "\x1b\\" };
        format!("\x1b]{n};{text}{end}").into_bytes()
    });
    let dcs = (params, "[ -~]{0,20}")
        .prop_map(|(params, data)| format!("\x1bP{params}q{data}\x1b\\").into_bytes());
    let esc = (intermediate_byte(), 0x30u8..=0x7E).prop_map(|(intermediate, end)| {
        let mut bytes = vec![0x1B];
        bytes.extend(intermediate);
        bytes.push(end);
        bytes
    });
    prop_oneof![4 => csi, 1 => osc, 1 => dcs, 1 => esc]
}

fn intermediate_byte() -> impl Strategy<Value = Option<u8>> {
    prop_oneof![3 => Just(None), 1 => (0x20u8..=0x2F).prop_map(Some)]
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn matches_vte_however_the_input_is_split(
        bytes in stream(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..8),
    ) {
        prop_assert_eq!(ours(&split(&bytes, &cuts)), vtes(&bytes));
    }

    #[test]
    fn matches_vte_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        prop_assert_eq!(ours(&[&bytes]), vtes(&bytes));
    }
}

#[test]
fn text_arrives_in_runs() {
    let events = ours(&[b"hello world\r\n"]);
    assert_eq!(
        events,
        [
            Event::Print("hello world".to_owned()),
            Event::Execute(b'\r'),
            Event::Execute(b'\n'),
        ]
    );
}

#[test]
fn csi_parameters_and_subparameters() {
    let events = ours(&[b"\x1b[1;38:2::10:20:30;;4m"]);
    assert_eq!(
        events,
        [Event::Csi {
            params: vec![vec![1], vec![38, 2, 0, 10, 20, 30], vec![0], vec![4]],
            intermediates: vec![],
            ignore: false,
            action: 'm',
        }]
    );
}

#[test]
fn a_split_character_is_put_back_together() {
    // The case vte gets wrong: a character finished by the next piece, which
    // goes on with more text and then another unfinished character.
    let events = ours(&[b"\xcc", b"\x81e\xcc", b"\x81"]);
    assert_eq!(events, [Event::Print("\u{301}e\u{301}".to_owned())]);
}

#[test]
fn osc_strings_are_capped() {
    let mut bytes = b"\x1b]2;".to_vec();
    bytes.resize(MAX_OSC_LEN * 2, b'x');
    bytes.push(0x07);
    let events = ours(&[&bytes]);
    let [Event::Osc { params, .. }] = &events[..] else {
        panic!("expected one OSC, got {} events", events.len());
    };
    assert_eq!(params[0], b"2");
    // The `2` takes one byte of the cap; separators take none.
    assert_eq!(params[1].len(), MAX_OSC_LEN - 1);
}

#[test]
fn osc_strings_with_many_parts_match_vte() {
    // More parts than are split out: the rest stay in the last one.
    let parts: Vec<String> = (0..25).map(|n| n.to_string()).collect();
    let bytes = format!("\x1b]{}\x07", parts.join(";")).into_bytes();
    assert_eq!(ours(&[&bytes]), vtes(&bytes));
}
