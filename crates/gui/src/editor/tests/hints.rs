//! Inlay hints: drawn between the text, muted, where the language server
//! says, but never part of it: the cursor, motions, searches, selections,
//! yanks and the mouse pass them by, and long lines wrap with them.

use std::fmt::Write;
use std::rc::Rc;
use std::time::Instant;

use clackatoo_protocol::{
    Answer, BufferUpdate, Color, InlayHint, InlayHintKind, Query, ServerFeatures, Size, Use,
    UseKind,
};
use clackatoo_term::Attributes;

use super::Setup;
use crate::editor::{HINTS_WAIT, USES_WAIT};
use crate::motion::Lines;
use crate::render::HighlightKind;

/// The text the tests show: a binding with a call, whose arguments the
/// server names, and another binding.
const TEXT: &str = "let total = add(left, right);\nlet x = 1;\nend\n";

/// What a server says of [`TEXT`]: the types of `total` and `x`, the names
/// of `add`'s parameters, padded after, and what the first line comes to,
/// after its end, padded before.
fn hints() -> Vec<InlayHint> {
    let hint = |at, label: &str, kind, (pad_left, pad_right)| InlayHint {
        at,
        label: label.into(),
        kind,
        pad_left,
        pad_right,
    };
    vec![
        hint(9, ": i32", InlayHintKind::Type, (false, false)),
        hint(16, "a:", InlayHintKind::Parameter, (false, true)),
        hint(22, "b:", InlayHintKind::Parameter, (false, true)),
        hint(29, "sum", InlayHintKind::Other, (true, false)),
        hint(35, ": u8", InlayHintKind::Type, (false, false)),
    ]
}

/// The first line, drawn with its hints, after its number.
const HINTED: &str = "  1 let total: i32 = add(a: left, b: right); sum";

impl Setup {
    /// Tells the editor its language server gives inlay hints.
    fn says_hints(&mut self) {
        self.copy.update(BufferUpdate::Features(ServerFeatures {
            inlay_hints: true,
            ..ServerFeatures::default()
        }));
    }

    /// Gives the copy `hints`, as the answer for bytes `start` to `end`.
    fn hinted(&mut self, (start, end): (usize, usize), hints: Vec<InlayHint>) {
        self.answer(0, Answer::InlayHints { start, end, hints });
    }

    /// [`TEXT`]'s hints, for all of it, in a pane wide enough for them,
    /// with lines numbered from the first.
    pub(super) fn with_hints() -> Self {
        let mut setup = Self::new(TEXT);
        setup.set(|options| options.relativenumber = false);
        setup.editor.resize(Size::new(60, 10).unwrap());
        setup.says_hints();
        setup.hinted((0, TEXT.len()), hints());
        setup
    }

    /// Lets the editor watch what it shows at `now`, as the desk does each
    /// time the window wakes; returns whether it asked.
    fn watch_hints(&mut self, now: Instant) -> bool {
        self.editor.settle(&self.copy);
        let asked = self.editor.watch_hints(now, true, &mut self.copy);
        self.serve();
        asked
    }

    /// The questions of inlay hints asked so far, as the bytes they're
    /// about.
    fn hints_asked(&self) -> Vec<(usize, usize)> {
        (self.queries.iter())
            .filter_map(|&(_, at, query)| match query {
                Query::InlayHints { end } => Some((at, end)),
                _ => None,
            })
            .collect()
    }

    /// The rows drawn, as text.
    fn drawn(&mut self) -> Vec<String> {
        self.editor.settle(&self.copy);
        let (rows, _) = self.editor.scene();
        rows.iter()
            .map(|row| row.text())
            .take_while(|row| row != "~")
            .collect()
    }

    /// Where the cursor's drawn, as a row and column of the pane.
    fn cursor_drawn(&mut self) -> (usize, usize) {
        self.editor.settle(&self.copy);
        let (_, cursor) = self.editor.scene();
        let cursor = cursor.expect("the cursor is in view");
        (cursor.line, cursor.col)
    }
}

#[test]
fn hints_are_drawn_between_the_text_muted_and_padded() {
    let mut setup = Setup::with_hints();
    assert_eq!(setup.drawn(), [HINTED, "  2 let x: u8 = 1;", "  3 end"]);
    let (rows, _) = setup.editor.scene();
    let style = |col: usize| *rows[0].style(rows[0].cells()[col]);
    // The type italic and grey; a parameter's name dimmer; its padding,
    // and the text, as they'd be.
    let (colon, a, pad, text) = (style(13), style(25), style(27), style(28));
    assert_eq!(colon.fg, Color::Indexed(8));
    assert!(colon.attributes.contains(Attributes::ITALIC));
    assert!(!colon.attributes.contains(Attributes::DIM));
    assert!(a.attributes.contains(Attributes::ITALIC | Attributes::DIM));
    assert_eq!(pad.attributes, Attributes::empty());
    assert!(!text.attributes.contains(Attributes::ITALIC));
}

#[test]
fn the_cursor_steps_over_hints_and_motions_see_only_the_text() {
    let mut setup = Setup::with_hints();
    // Each stop, its cell in the line, and where it's drawn: after the
    // hints before it.
    for (keys, col, drawn) in [
        ("w", 4, 8),
        ("e", 8, 12),
        // The blank after `total`, which the type is drawn before.
        ("l", 9, 18),
        ("w", 10, 19),
        ("w", 12, 21),
        ("w", 15, 24),
        // `left`, after the parameter's name and its padding.
        ("w", 16, 28),
        ("b", 15, 24),
        // The last character, before what the line comes to.
        ("$", 28, 43),
        ("l", 28, 43),
    ] {
        setup.keys(keys);
        assert_eq!(setup.cursor(), (0, col), "{keys}");
        assert_eq!(setup.cursor_drawn(), (0, drawn), "{keys}");
    }
    // A search looks only in the text.
    setup.keys("0/i32<CR>");
    assert_eq!(setup.cursor(), (0, 0));
    setup.keys("/right<CR>");
    assert_eq!(setup.cursor(), (0, 22));
    // Yanked, the line is its text.
    setup.keys("yy");
    assert_eq!(setup.register().text, "let total = add(left, right);\n");
    // `x` deletes the text's character, and the hint before it goes, as
    // it may be wrong now; the rest move.
    setup.keys("x");
    assert_eq!(
        setup.text(),
        "let total = add(left, ight);\nlet x = 1;\nend\n"
    );
    assert_eq!(
        setup.drawn()[0],
        "  1 let total: i32 = add(a: left, ight); sum"
    );
    // A line deleted takes its hints with it.
    setup.keys("dd");
    assert_eq!(setup.drawn(), ["  1 let x: u8 = 1;", "  2 end"]);
}

#[test]
fn a_selection_across_a_hint_lights_the_text_and_yanks_only_it() {
    let mut setup = Setup::with_hints();
    setup.keys("wvw");
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, 8..13), (0, 18..20)]
    );
    setup.keys("y");
    assert_eq!(setup.register().text, "total =");
}

#[test]
fn hints_at_one_place_are_passed_over_together_by_selections() {
    let mut setup = Setup::with_hints();
    let hint = |at, label: &str, pad_left| InlayHint {
        at,
        label: label.into(),
        kind: InlayHintKind::Type,
        pad_left,
        pad_right: false,
    };
    // Two before the blank after `total`, and two after the line's end.
    setup.hinted(
        (0, 29),
        vec![
            hint(9, ": i32", false),
            hint(9, "!", false),
            hint(29, "sum", true),
            hint(29, "x", false),
        ],
    );
    assert_eq!(
        setup.drawn()[0],
        "  1 let total: i32! = add(left, right); sumx"
    );
    setup.keys("wvw");
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, 8..13), (0, 19..21)]
    );
    // A whole line lights its text, around both pairs, and the pane after
    // the line's.
    setup.keys("<Esc>V");
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, 4..13), (0, 19..39), (0, 44..60)]
    );
}

#[test]
fn a_match_found_and_the_uses_of_a_name_light_the_text_around_hints() {
    let mut setup = Setup::with_hints();
    setup.keys("/total =<CR>");
    assert_eq!(
        setup.highlighted(HighlightKind::Match),
        [(0, 8..13), (0, 18..20)]
    );
    // The uses of `left`, after the parameter's name, and of `total`,
    // before its type.
    setup.keys("<Esc>/left<CR><Esc>");
    setup.copy.update(BufferUpdate::Features(ServerFeatures {
        inlay_hints: true,
        uses: true,
        ..ServerFeatures::default()
    }));
    let now = Instant::now();
    setup.editor.watch_uses(now, true, &mut setup.copy);
    setup
        .editor
        .watch_uses(now + USES_WAIT, true, &mut setup.copy);
    setup.serve();
    let id = (setup.queries.iter())
        .rev()
        .find_map(|&(id, _, query)| (query == Query::Uses).then_some(id))
        .expect("asked where `left` is used");
    let used = |start, end, kind| Use { start, end, kind };
    setup.answer(
        id,
        Answer::Uses(vec![
            used(4, 9, UseKind::Write),
            used(16, 20, UseKind::Read),
        ]),
    );
    assert_eq!(setup.highlighted(HighlightKind::Written), [(0, 8..13)]);
    assert_eq!(setup.highlighted(HighlightKind::Used), [(0, 28..32)]);
}

#[test]
fn colorcolumn_runs_straight_down_past_hints() {
    let mut setup = Setup::with_hints();
    // The 20th column, counting from 1.
    setup.set(|options| options.colorcolumn = vec![20]);
    assert_eq!(
        setup.highlighted(HighlightKind::ColorColumn),
        [(0, 23..24), (1, 23..24), (2, 23..24)]
    );
}

#[test]
fn long_lines_wrap_with_their_hints_which_never_split_from_their_characters() {
    let mut setup = Setup::with_hints();
    // 20 cells for text.
    setup.editor.resize(Size::new(24, 10).unwrap());
    // Between words, a hint goes with the word it's before.
    assert_eq!(
        setup.drawn()[..3],
        [
            "  1 let total: i32 =",
            "    add(a: left,",
            "    b: right); sum"
        ]
    );
    setup.set(|options| options.linebreak = false);
    assert_eq!(
        setup.drawn(),
        [
            "  1 let total: i32 = add",
            // `(` and `a: left` don't fit, and go on together.
            "    (a: left, b: right)",
            // Nor does what the line comes to, after its end, which goes
            // with its last character.
            "    ; sum",
            "  2 let x: u8 = 1;",
            "  3 end",
        ]
    );
    setup.keys("f(");
    assert_eq!(setup.cursor_drawn(), (1, 4));
    setup.keys("l");
    assert_eq!(setup.cursor_drawn(), (1, 8));
    // Down a screen row, to the last character, and on to the next line.
    setup.keys("gj");
    assert_eq!(setup.cursor(), (0, 28));
    setup.keys("gj");
    assert_eq!(setup.cursor().0, 1);
}

#[test]
fn nowrap_scrolls_far_enough_to_show_the_cursor_past_hints() {
    let mut setup = Setup::with_hints();
    setup.set(|options| options.wrap = false);
    // 26 cells for text.
    setup.editor.resize(Size::new(30, 10).unwrap());
    setup.keys("$");
    let (row, col) = setup.cursor_drawn();
    assert_eq!(row, 0);
    assert!(col < 30, "{col}");
    assert_eq!(
        setup.drawn()[0].chars().nth(col),
        Some(';'),
        "{:?}",
        setup.drawn()
    );
}

#[test]
fn a_click_on_a_hint_puts_the_cursor_on_the_character_its_drawn_before() {
    let mut setup = Setup::with_hints();
    for (col, at) in [
        // On the type, `: i32`: the blank after `total`.
        (15, 9),
        // On `a: `, its padding too: `left`.
        (25, 16),
        (27, 16),
        // On the text between hints.
        (30, 18),
        // On what the line comes to, after its end: the last character.
        (46, 28),
        (2, 0),
    ] {
        setup.editor.click(0, col, &mut setup.copy);
        assert_eq!(setup.cursor(), (0, at), "a click at {col}");
    }
}

#[test]
fn lines_without_hints_arent_laid_out_or_wrapped_again_when_hints_come() {
    let mut setup = Setup::new(TEXT);
    setup.editor.resize(Size::new(60, 10).unwrap());
    setup.says_hints();
    setup.drawn();
    let text = setup.editor.text(&setup.copy);
    let (laid, rows) = (text.laid(2).unwrap(), text.screen_rows(2));
    let first = text.screen_rows(0);
    drop(text);
    setup.hinted((0, TEXT.len()), hints());
    setup.drawn();
    let text = setup.editor.text(&setup.copy);
    assert!(Rc::ptr_eq(&laid, &text.laid(2).unwrap()), "laid out again");
    assert!(Rc::ptr_eq(&rows, &text.screen_rows(2)), "wrapped again");
    // The line with hints is wrapped again, with them...
    let hinted = text.screen_rows(0);
    assert!(!Rc::ptr_eq(&first, &hinted));
    drop(text);
    // ...but not for the same hints, answered again.
    let mut heard = setup.copy.update(BufferUpdate::Answer {
        id: 1,
        version: setup.daemon.version(),
        answer: Answer::InlayHints {
            start: 0,
            end: TEXT.len(),
            hints: hints(),
        },
    });
    assert_eq!(
        heard,
        crate::editor::Heard::Hinted {
            id: 1,
            changed: false
        }
    );
    setup.editor.heard(&mut heard, true, &mut setup.copy);
    let text = setup.editor.text(&setup.copy);
    assert!(Rc::ptr_eq(&hinted, &text.screen_rows(0)));
}

#[test]
fn the_cursors_line_shows_no_hints_while_typing_but_the_others_do() {
    let mut setup = Setup::with_hints();
    setup.keys("A");
    assert_eq!(
        setup.drawn(),
        [
            "  1 let total = add(left, right);",
            "  2 let x: u8 = 1;",
            "  3 end"
        ]
    );
    // Typing at the end, the cursor is just after the text.
    assert_eq!(setup.cursor_drawn(), (0, 4 + 29));
    setup.keys("<Esc>");
    assert_eq!(setup.drawn()[0], HINTED);
}

#[test]
fn hints_turned_off_are_drawn_nowhere_and_asked_for_no_more() {
    let mut setup = Setup::with_hints();
    setup.set(|options| options.inlayhints = false);
    assert_eq!(setup.drawn()[0], "  1 let total = add(left, right);");
    let now = Instant::now();
    assert!(!setup.watch_hints(now));
    assert!(!setup.watch_hints(now + HINTS_WAIT));
    assert_eq!(setup.hints_asked(), []);
    setup.set(|options| options.inlayhints = true);
    assert_eq!(setup.drawn()[0], HINTED);
    assert!(!setup.watch_hints(now));
    assert!(setup.watch_hints(now + HINTS_WAIT));
    assert_eq!(setup.hints_asked().len(), 1);
}

#[test]
fn hints_are_asked_for_what_is_shown_and_a_screenful_either_side_once_things_rest() {
    // 100 lines, in a pane 10 high.
    let text = (0..100).fold(String::new(), |mut text, n| {
        let _ = writeln!(text, "let v{n} = {n};");
        text
    });
    let line_start = |line: usize| text.split_inclusive('\n').take(line).map(str::len).sum();
    let line_end = |line: usize| line_start(line + 1) - 1;
    let mut setup = Setup::new(&text);
    let now = Instant::now();
    // Not of a server that doesn't say it gives them.
    assert!(!setup.watch_hints(now));
    assert!(!setup.watch_hints(now + HINTS_WAIT));
    setup.says_hints();
    assert!(!setup.watch_hints(now), "not at once");
    assert!(setup.watch_hints(now + HINTS_WAIT));
    assert_eq!(setup.hints_asked(), [(0, line_end(19))]);
    // Nothing more while nothing changes, nor scrolling inside what was
    // asked about.
    assert!(!setup.watch_hints(now + HINTS_WAIT * 2));
    setup.editor.wheel(-5, &setup.copy);
    assert!(!setup.watch_hints(now + HINTS_WAIT * 3));
    assert!(!setup.watch_hints(now + HINTS_WAIT * 4));
    // Scrolling out of it asks once the view rests.
    setup.editor.wheel(-10, &setup.copy);
    assert!(!setup.watch_hints(now + HINTS_WAIT * 5));
    assert!(setup.watch_hints(now + HINTS_WAIT * 6));
    assert_eq!(setup.hints_asked()[1], (line_start(5), line_end(34)));
    // An edit asks again, once typing pauses.
    setup.keys("x");
    assert!(!setup.watch_hints(now + HINTS_WAIT * 7));
    setup.keys("x");
    assert!(!setup.watch_hints(now + HINTS_WAIT * 7 + HINTS_WAIT / 2));
    assert!(setup.watch_hints(now + HINTS_WAIT * 9));
    assert_eq!(setup.hints_asked().len(), 3);
    // As it does when the server says its hints may be out of date.
    setup.says_hints();
    assert!(!setup.watch_hints(now + HINTS_WAIT * 10));
    assert!(setup.watch_hints(now + HINTS_WAIT * 11));
    assert_eq!(setup.hints_asked().len(), 4);
    // And when the copy lets hints go, having too many, as another pane's
    // answer can make it: they may have been this one's.
    let far = |at| InlayHint {
        at,
        label: "x".into(),
        kind: InlayHintKind::Type,
        pad_left: false,
        pad_right: false,
    };
    setup.hinted(
        (1 << 20, 1 << 21),
        (0..10_000).map(|n| far((1 << 20) + n)).collect(),
    );
    assert!(!setup.watch_hints(now + HINTS_WAIT * 12));
    setup.hinted((0, 0), vec![far(0)]);
    assert!(!setup.watch_hints(now + HINTS_WAIT * 13));
    assert!(setup.watch_hints(now + HINTS_WAIT * 14));
    assert_eq!(setup.hints_asked().len(), 5);
}

#[test]
fn hints_move_with_edits_made_elsewhere_in_the_file() {
    let mut setup = Setup::with_hints();
    // A line put in above: the hints stay with their text.
    setup.keys("Onew<Esc>");
    assert_eq!(
        setup.drawn(),
        [
            "  1 new",
            &HINTED.replace("  1", "  2"),
            "  3 let x: u8 = 1;",
            "  4 end"
        ]
    );
    // An answer for part of the file leaves the rest as it was.
    let line = "new\nlet total = add(left, right);\n".len();
    setup.hinted((line, line + 10), Vec::new());
    assert_eq!(setup.drawn()[2], "  3 let x = 1;");
    assert_eq!(setup.drawn()[1], HINTED.replace("  1", "  2"));
}
