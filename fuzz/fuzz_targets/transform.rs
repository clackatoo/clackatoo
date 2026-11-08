//! Edits made at once to the same text, reconciled: whichever is made
//! first, each made after the other as transformed gives the same text.
//! Composing, inverting, coarsening and moving places through edits must
//! agree with making them, and an edit read from the wire, whatever its
//! pieces, must be refused rather than panic when it doesn't fit.

#![no_main]

use arbitrary::Arbitrary;
use clackatoo_text::{Bias, Edit, Op, Rope, compose, transform};
use libfuzzer_sys::fuzz_target;

/// A change to a text, at a place and of a length that are taken modulo
/// the text's and snapped to characters.
#[derive(Arbitrary, Debug)]
struct Change {
    at: u16,
    removed: u8,
    inserted: String,
}

#[derive(Arbitrary, Debug)]
struct Input {
    text: String,
    a: Vec<Change>,
    b: Vec<Change>,
    /// Pieces as a peer may send them, unchecked.
    raw: Vec<RawOp>,
    places: Vec<u16>,
    most: u8,
}

#[derive(Arbitrary, Debug)]
enum RawOp {
    Retain(u16),
    Insert(String),
    Delete(String),
}

/// The edit making `changes` to `text`, in order of place, those that
/// overlap one before left out.
fn edit(text: &str, changes: &[Change]) -> Edit {
    let snap = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let mut places: Vec<(usize, usize, &str)> = (changes.iter())
        .map(|change| {
            let at = snap(usize::from(change.at) % (text.len() + 1));
            (
                at,
                snap(at + usize::from(change.removed)),
                change.inserted.as_str(),
            )
        })
        .collect();
    places.sort_by_key(|&(at, end, _)| (at, end));
    let mut made = Edit::new();
    let mut done = 0;
    for (at, end, inserted) in places {
        if at < done {
            continue;
        }
        made.retain(at - done)
            .delete(&text[at..end])
            .insert(inserted);
        done = end;
    }
    made.retain(text.len() - done);
    made
}

fn made(edit: &Edit, text: &str) -> String {
    let string = edit.apply_to(text).expect("the edit fits its text");
    let mut rope = Rope::from_str(text);
    edit.apply(&mut rope).expect("the edit fits its rope");
    assert_eq!(rope.to_string(), string);
    string
}

fuzz_target!(|input: Input| {
    let text = input.text.as_str();
    let (a, b) = (edit(text, &input.a), edit(text, &input.b));
    let (after_a, after_b) = (made(&a, text), made(&b, text));

    // Converging.
    let (a_then, b_then) = transform(&a, &b).expect("edits of one text transform");
    let both = made(&b_then, &after_a);
    assert_eq!(made(&a_then, &after_b), both);

    // Composed, the same as one after the other, either way round.
    let ab = compose(&a, &b_then).expect("b follows a");
    let ba = compose(&b, &a_then).expect("a follows b");
    assert_eq!(made(&ab, text), both);
    assert_eq!(made(&ba, text), both);

    // Undone.
    let undo = a.invert();
    assert_eq!(made(&undo, &after_a), text);
    // Text it took away and put back is still taken away and put back.
    let none = compose(&a, &undo).expect("its inverse follows it");
    assert_eq!(made(&none, text), text);

    // Coarsened, the same change.
    let rope = Rope::from_str(text);
    assert_eq!(
        made(&a.coarsened(&rope, usize::from(input.most)), text),
        after_a
    );

    // Places, through it, land in the text it makes.
    let mut places: Vec<usize> = (input.places.iter())
        .map(|&place| usize::from(place) % (text.len() + 1))
        .collect();
    places.sort_unstable();
    let mut untouched = a.untouched();
    for &place in &places {
        for bias in [Bias::Before, Bias::After] {
            assert!(a.map(place, bias) <= after_a.len());
            assert!(a.map_keeping(place, bias) <= after_a.len());
        }
        if let Some(moved) = untouched.place(place) {
            assert!(moved <= after_a.len());
        }
        let _ = a.map_untouched(place, place + 1);
    }
    if let Some(moved) = a.moved() {
        let _ = moved.is_lines(&Rope::from_str(&after_a));
    }

    // Pieces as sent: refused if they don't fit, never a panic.
    let ops: Vec<Op> = (input.raw.into_iter())
        .map(|op| match op {
            RawOp::Retain(len) => Op::Retain(usize::from(len)),
            RawOp::Insert(text) => Op::Insert(text),
            RawOp::Delete(text) => Op::Delete(text),
        })
        .collect();
    if let Ok(sent) = Edit::try_from(ops) {
        let mut rope = Rope::from_str(text);
        if sent.apply(&mut rope).is_ok() {
            assert_eq!(
                rope.to_string(),
                sent.apply_to(text).expect("it fit the rope")
            );
        } else {
            assert!(sent.apply_to(text).is_err());
        }
        if let Ok((sent_then, a_after)) = transform(&sent, &a)
            && let Ok(sent_made) = sent.apply_to(text)
        {
            assert_eq!(made(&a_after, &sent_made), made(&sent_then, &after_a));
        }
    }
});
