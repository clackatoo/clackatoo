//! The other uses of the name the cursor rests on, lit once it's rested
//! there; and part of a file formatted, by `gq` on a selection and by
//! `:Format` over a range.

use std::time::Instant;

use clackatoo_protocol::{Answer, BufferUpdate, Query, ServerFeatures, Use, UseKind};

use super::Setup;
use crate::editor::uses::USES_WAIT;
use crate::render::HighlightKind;

/// Cells lit, as the rows they're on and the cells on each.
type Cells = Vec<(usize, std::ops::Range<usize>)>;

impl Setup {
    /// Tells the editor its language server says where names are used.
    fn says_uses(&mut self) {
        self.copy.update(BufferUpdate::Features(ServerFeatures {
            uses: true,
            ..ServerFeatures::default()
        }));
    }

    /// Lets the editor watch the cursor at `now`, as the desk does each
    /// time the window wakes, with the focus if `focused`; returns whether
    /// it asked anything, or let marks go.
    fn watch(&mut self, now: Instant, focused: bool) -> bool {
        let done = self.editor.watch_uses(now, focused, &mut self.copy);
        self.serve();
        done
    }

    /// Lets the cursor rest where it is, as long as it takes to ask.
    fn rest(&mut self) {
        let now = Instant::now();
        self.watch(now, true);
        self.watch(now + USES_WAIT, true);
    }

    /// The questions of where a name is used asked so far: their numbers
    /// and places.
    fn uses_asked(&self) -> Vec<(u64, usize)> {
        (self.queries.iter())
            .filter(|(_, _, query)| *query == Query::Uses)
            .map(|&(id, at, _)| (id, at))
            .collect()
    }

    /// The uses lit, written and read, as rows and their cells.
    fn lit(&mut self) -> (Cells, Cells) {
        (
            self.highlighted(HighlightKind::Written),
            self.highlighted(HighlightKind::Used),
        )
    }
}

/// The text the tests rest on: `x` written on the first line, read twice
/// on the second, and named in a comment on the third.
const TEXT: &str = "let x = 1;\nx + x\n// x\n";

/// What a server says of `x` in [`TEXT`].
fn uses_of_x() -> Answer {
    let used = |start, kind| Use {
        start,
        end: start + 1,
        kind,
    };
    Answer::Uses(vec![
        used(4, UseKind::Write),
        used(11, UseKind::Read),
        used(15, UseKind::Read),
        used(20, UseKind::Text),
    ])
}

#[test]
fn the_uses_of_a_name_are_lit_once_the_cursor_rests_on_it() {
    let mut setup = Setup::new(TEXT);
    setup.says_uses();
    setup.keys("w");
    // Not at once, nor before it's rested long enough: moving through the
    // text asks nothing.
    let now = Instant::now();
    setup.watch(now, true);
    setup.watch(now + USES_WAIT / 2, true);
    assert!(setup.uses_asked().is_empty());
    assert!(setup.editor.wakes().is_some(), "the window wakes to ask");
    assert!(setup.watch(now + USES_WAIT, true), "asked");
    assert_eq!(setup.uses_asked(), [(setup.uses_asked()[0].0, 4)]);
    assert_eq!(setup.editor.wakes(), None, "asked once");
    let (id, _) = setup.uses_asked()[0];
    setup.answer(id, uses_of_x());
    // Where it's written apart from where it's read; the comment's, which
    // the server says nothing more of, as where it's read.
    let (written, read) = setup.lit();
    assert_eq!(written.len(), 1);
    let (row, cells) = &written[0];
    assert_eq!((*row, cells.len()), (0, 1));
    let x = cells.start - 4;
    assert_eq!(read, [(1, x..x + 1), (1, x + 4..x + 5), (2, x + 3..x + 4)]);
    // Resting on still, nothing more is asked.
    let later = Instant::now() + USES_WAIT * 4;
    assert!(!setup.watch(later, true));
    assert_eq!(setup.uses_asked().len(), 1);
}

#[test]
fn the_uses_lit_go_once_the_cursor_leaves_the_name_or_the_text_or_mode_changes() {
    let mut setup = Setup::new(TEXT);
    setup.says_uses();
    let lit_up = |setup: &mut Setup| {
        setup.rest();
        let (id, _) = *setup.uses_asked().last().expect("asked");
        setup.answer(id, uses_of_x());
        assert_eq!(setup.lit().1.len(), 3, "lit");
    };
    setup.keys("w");
    lit_up(&mut setup);
    // Off the name, they go at once, before the window's next look.
    setup.keys("l");
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    // Back on it, they're asked for afresh.
    setup.keys("h");
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    lit_up(&mut setup);
    assert_eq!(setup.uses_asked().len(), 2);

    // Another's edit: they're placed in the text before it.
    let end = setup.copy.text().len_bytes();
    let update = setup.daemon.agent(end, "x\n");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(update);
    setup.editor.heard(&mut heard, true, &mut setup.copy);
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    lit_up(&mut setup);

    // Visual mode, and insert mode, where nothing's asked however long the
    // cursor rests.
    setup.keys("v");
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    setup.keys("<Esc>");
    lit_up(&mut setup);
    let asked = setup.uses_asked().len();
    setup.keys("i");
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    setup.rest();
    assert_eq!(setup.uses_asked().len(), asked);
    setup.keys("<Esc>");

    // Nor is anything asked by an editor without the focus, or with the
    // option off, which lets the marks go.
    setup.keys("w");
    let now = Instant::now();
    setup.watch(now, false);
    setup.watch(now + USES_WAIT, false);
    assert_eq!(setup.uses_asked().len(), asked);
    lit_up(&mut setup);
    setup.set(|options| options.highlightuses = false);
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    setup.keys("w");
    setup.rest();
    assert_eq!(setup.uses_asked().len(), asked + 1);
}

#[test]
fn uses_are_asked_only_on_a_name_of_a_server_that_says() {
    let mut setup = Setup::new(TEXT);
    // A server that doesn't say.
    setup.keys("w");
    setup.rest();
    assert!(setup.uses_asked().is_empty());
    // On a blank, or `=`, there's no name.
    setup.says_uses();
    setup.keys("l");
    setup.rest();
    setup.keys("l");
    setup.rest();
    assert!(setup.uses_asked().is_empty());
}

#[test]
fn an_answer_about_where_the_cursor_was_lights_nothing() {
    let mut setup = Setup::new(TEXT);
    setup.says_uses();
    setup.keys("w");
    setup.rest();
    let (first, _) = setup.uses_asked()[0];
    // On to the next name, which is asked about in turn: the first answer
    // is let go, as the daemon tells the server it needn't answer.
    setup.keys("j0");
    setup.rest();
    let (second, at) = setup.uses_asked()[1];
    assert_eq!(at, 11);
    setup.answer(first, uses_of_x());
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    setup.answer(second, uses_of_x());
    assert_eq!(setup.lit().1.len(), 3);

    // An answer that comes once the cursor's left the name lights nothing,
    // nor does one that failed.
    setup.keys("k");
    setup.rest();
    let (third, _) = *setup.uses_asked().last().unwrap();
    setup.keys("$");
    setup.answer(third, uses_of_x());
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    setup.keys("0w");
    setup.rest();
    let (fourth, _) = *setup.uses_asked().last().unwrap();
    setup.answer(fourth, Answer::Failed("no".into()));
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    assert_eq!(setup.editor.status, None, "nothing said of it");
}

#[test]
fn thousands_of_uses_light_only_those_in_view() {
    let text = "x\n".repeat(5000);
    let mut setup = Setup::new(&text);
    setup.says_uses();
    setup.rest();
    let (id, _) = setup.uses_asked()[0];
    let uses = (0..5000)
        .map(|line| Use {
            start: line * 2,
            end: line * 2 + 1,
            kind: UseKind::Read,
        })
        .collect();
    setup.answer(id, Answer::Uses(uses));
    // The pane is 10 rows high.
    let (_, read) = setup.lit();
    let rows: Vec<usize> = read.iter().map(|(row, _)| *row).collect();
    assert_eq!(rows, (0..10).collect::<Vec<_>>());
}

#[test]
fn gq_on_a_selection_is_formatted_as_its_language_server_does() {
    let mut setup = Setup::new("one\n  two\nthree\n");
    setup.keys("jVj");
    setup.keys("gq");
    // From the first line's start to the last one's end, for the desk to
    // ask, leaving visual mode, the cursor at its start.
    assert_eq!(setup.editor.take_format_asked(), Some((4, 15)));
    assert_eq!(setup.editor.mode, crate::pane::Mode::Normal);
    assert_eq!(setup.cursor(), (1, 0));
    // A selection of characters, exactly.
    setup.keys("lvl");
    setup.keys("gq");
    let byte = |setup: &mut Setup| setup.editor.take_format_asked();
    assert_eq!(byte(&mut setup), Some((5, 7)));
    // `gq` in normal mode is nothing, as yet.
    setup.keys("gq");
    assert_eq!(byte(&mut setup), None);
}

#[test]
fn format_over_a_range_formats_its_lines() {
    let mut setup = Setup::new("one\n  two\nthree\n");
    let ex = |setup: &mut Setup, line: &str| {
        setup
            .editor
            .ex(line, &mut setup.copy, &mut setup.shared, false)
            .map(|_| ())
    };
    // The last selection's lines, as `:'<,'>Format` is typed after `V`.
    setup.keys("jVj<Esc>");
    ex(&mut setup, "'<,'>Format").unwrap();
    assert_eq!(setup.editor.take_format_asked(), Some((4, 15)));
    ex(&mut setup, "1Format").unwrap();
    assert_eq!(setup.editor.take_format_asked(), Some((0, 3)));
    ex(&mut setup, "%Format").unwrap();
    assert_eq!(setup.editor.take_format_asked(), Some((0, 15)));
    assert_eq!(
        ex(&mut setup, "1Format now"),
        Err(":Format takes nothing after it, but has \"now\"".into())
    );
    assert_eq!(
        ex(&mut setup, "g/one/Format"),
        Err(":Format can't be done by a :g".into())
    );
    assert_eq!(setup.editor.take_format_asked(), None);
}

#[test]
fn an_answer_that_comes_once_the_focus_or_the_bar_has_the_keys_lights_nothing() {
    let mut setup = Setup::new(TEXT);
    setup.says_uses();
    setup.keys("w");
    // The focus goes while the question's out: the window looks again,
    // and the answer, when it comes, is for a pane without it.
    setup.rest();
    let (id, _) = *setup.uses_asked().last().unwrap();
    setup.watch(Instant::now(), false);
    setup.answer(id, uses_of_x());
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    // Or it comes before the window looks: the desk says the pane hasn't
    // the focus.
    setup.rest();
    let (id, _) = *setup.uses_asked().last().unwrap();
    let mut heard = (setup.copy).update(BufferUpdate::Answer {
        id,
        version: setup.daemon.version(),
        answer: uses_of_x(),
    });
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
    // Or a search is being typed in the bar.
    setup.rest();
    let (id, _) = *setup.uses_asked().last().unwrap();
    setup.keys("/");
    setup.answer(id, uses_of_x());
    assert_eq!(setup.lit(), (Vec::new(), Vec::new()));
}

#[test]
fn a_cursor_resting_before_the_server_has_started_asks_once_it_says() {
    let mut setup = Setup::new(TEXT);
    setup.keys("w");
    setup.rest();
    assert!(setup.uses_asked().is_empty());
    // The server starts, and says where names are used: the cursor,
    // resting as it was, asks.
    setup.says_uses();
    setup.rest();
    assert_eq!(setup.uses_asked().len(), 1);
}

#[test]
fn a_use_that_starts_above_the_view_is_lit_where_it_reaches_into_it() {
    let text = "x\n".repeat(40);
    let mut setup = Setup::new(&text);
    setup.says_uses();
    // Line 30, scrolled to.
    setup.keys("30G");
    setup.rest();
    let (id, at) = setup.uses_asked()[0];
    assert_eq!(at, 29 * 2);
    // One from line 2 to line 36's start, and the name under the cursor.
    let uses = vec![
        Use {
            start: 2,
            end: 35 * 2,
            kind: UseKind::Write,
        },
        Use {
            start: at,
            end: at + 1,
            kind: UseKind::Read,
        },
    ];
    setup.answer(id, Answer::Uses(uses));
    let top = setup.editor.top.line;
    assert!(top > 2, "scrolled past its start");
    let (written, read) = setup.lit();
    let rows: Vec<usize> = written.iter().map(|(row, _)| *row).collect();
    // Every row of a line it covers, from the top of the view, 10 rows
    // high.
    let covered = usize::try_from(35 - top).unwrap().min(10);
    assert_eq!(rows, (0..covered).collect::<Vec<_>>());
    assert_eq!(read.len(), 1);
}
