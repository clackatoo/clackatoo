//! Selecting with the mouse: dragging selects in visual mode, by
//! characters, words or lines, through wrapped rows and past inlay hints,
//! scrolling the view past its edges while the button is held.

use std::fmt::Write;
use std::time::{Duration, Instant};

use clackatoo_protocol::Size;

use super::{Mode, Setup};
use crate::motion::Point;
use crate::vim::Selection;

/// The four cells of line numbers left of the text, in a [`Setup`].
const GUTTER: i64 = 4;

/// A selection, as [`Setup::selected`] tells it.
type Selected = ((u64, usize), (u64, usize), Selection);

impl Setup {
    /// Presses the button at row `line` and column `col` of the text, right
    /// of the gutter, the `clicks`th time in a row.
    fn press(&mut self, line: i64, col: i64, clicks: u8) {
        let at = |value: i64| usize::try_from(value).unwrap();
        self.editor
            .press((at(line), at(col + GUTTER)), clicks, &mut self.copy);
        // As the desk notes what it ended or began, for `gv`.
        (self.editor).note_selection(&self.copy, &mut self.shared.marks);
    }

    /// Drags to row `line` and column `col` of the text, as of `now`.
    /// Returns whether the window must be redrawn.
    fn drag(&mut self, line: i64, col: i64, now: Instant) -> bool {
        self.editor
            .drag_to((line, col + GUTTER), now, &mut self.copy)
    }

    /// Lets go: what's selected, for the primary selection.
    fn release(&mut self) -> Option<String> {
        let selected = self.editor.release(true, &self.copy);
        (self.editor).note_selection(&self.copy, &mut self.shared.marks);
        selected
    }

    /// The selection: where it started and where the cursor is, as lines
    /// and columns, and by what.
    fn selected(&self) -> Option<Selected> {
        let Mode::Visual(kind) = self.editor.mode else {
            return None;
        };
        let anchor = self.editor.anchor?;
        let at = self.editor.cursor.at;
        Some(((anchor.line, anchor.col), (at.line, at.col), kind))
    }
}

/// A text of `lines` lines, each saying which it is, from 0.
fn numbered(lines: usize) -> String {
    (0..lines).fold(String::new(), |mut text, line| {
        let _ = writeln!(text, "line {line}");
        text
    })
}

#[test]
fn dragging_selects_from_the_press_to_the_mouse_either_way() {
    let mut setup = Setup::new("hello world\nsecond line\nthird\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(0, 1, 1);
    assert_eq!(setup.selected(), None, "a press selects nothing");
    assert!(setup.drag(0, 6, now));
    assert_eq!(
        setup.selected(),
        Some(((0, 1), (0, 6), Selection::Characters))
    );
    // The same cell again changes nothing, and isn't drawn again.
    assert!(!setup.drag(0, 6, now));
    assert!(setup.drag(1, 3, now));
    assert_eq!(
        setup.selected(),
        Some(((0, 1), (1, 3), Selection::Characters))
    );
    // Back past the press: from it, backwards.
    assert!(setup.drag(0, 0, now));
    assert_eq!(
        setup.selected(),
        Some(((0, 1), (0, 0), Selection::Characters))
    );
    assert_eq!(setup.release().as_deref(), Some("he"));
    // Letting go leaves it selected.
    assert_eq!(
        setup.selected(),
        Some(((0, 1), (0, 0), Selection::Characters))
    );
}

#[test]
fn dragging_with_alt_selects_a_block_whose_rows_go_to_the_primary_selection() {
    let mut setup = Setup::new("hello world\nsecond line\nthird\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    let at = |value: i64| usize::try_from(value).unwrap();
    (setup.editor).press_with((0, at(1 + GUTTER)), 1, true, &mut setup.copy);
    assert_eq!(setup.selected(), None, "a press selects nothing");
    assert!(setup.drag(2, 3, now));
    assert_eq!(setup.selected(), Some(((0, 1), (2, 3), Selection::Block)));
    // The same columns of each line, as many as it has.
    assert_eq!(setup.release().as_deref(), Some("ell\neco\nhir"));
    assert_eq!(setup.selected(), Some(((0, 1), (2, 3), Selection::Block)));
    // `d` takes it, as from keys.
    setup.keys("d");
    assert_eq!(setup.text(), "ho world\nsnd line\ntd\n");
}

#[test]
fn a_plain_click_moves_the_cursor_and_ends_a_selection() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(0, 0, 1);
    setup.drag(0, 4, now);
    setup.release();
    assert!(setup.selected().is_some());
    setup.press(1, 2, 1);
    assert_eq!(setup.selected(), None);
    assert_eq!(setup.editor.mode, Mode::Normal);
    assert_eq!(setup.cursor(), (1, 2));
    // Moving within the character clicked selects nothing either.
    assert!(!setup.drag(1, 2, now));
    assert_eq!(setup.release(), None, "nothing for the primary selection");
    assert_eq!(setup.selected(), None);
    // Nor does a selection made with keys outlast a click.
    setup.keys("vj");
    setup.press(0, 3, 1);
    setup.release();
    assert_eq!(setup.selected(), None);
}

#[test]
fn past_a_line_the_drag_takes_its_last_character_and_on_the_numbers_its_first() {
    let mut setup = Setup::new("ab\nlonger line\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(1, 3, 1);
    setup.drag(0, 30, now);
    assert_eq!(
        setup.selected(),
        Some(((1, 3), (0, 1), Selection::Characters))
    );
    setup.drag(1, -3, now);
    assert_eq!(
        setup.selected(),
        Some(((1, 3), (1, 0), Selection::Characters))
    );
}

#[test]
fn a_drag_goes_down_the_rows_a_long_line_goes_on_over() {
    // 36 cells for text: the first line goes on over three rows.
    let mut setup = Setup::new(&format!("{}\nshort\n", "a".repeat(80)));
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(0, 2, 1);
    setup.drag(1, 3, now);
    assert_eq!(
        setup.selected(),
        Some(((0, 2), (0, 39), Selection::Characters))
    );
    setup.drag(2, 1, now);
    assert_eq!(
        setup.selected(),
        Some(((0, 2), (0, 73), Selection::Characters))
    );
    // Past the line's last row, its last character; then the next line.
    setup.drag(2, 30, now);
    assert_eq!(
        setup.selected(),
        Some(((0, 2), (0, 79), Selection::Characters))
    );
    setup.drag(3, 2, now);
    assert_eq!(
        setup.selected(),
        Some(((0, 2), (1, 2), Selection::Characters))
    );
    assert_eq!(
        setup.release().map(|text| text.len()),
        Some(78 + 1 + 3),
        "from the third a, through the line break, to `sho`"
    );
}

#[test]
fn a_double_click_selects_a_word_and_its_drag_whole_words_either_way() {
    let mut setup = Setup::new("one two-three four\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(0, 5, 2);
    assert_eq!(
        setup.selected(),
        Some(((0, 4), (0, 6), Selection::Characters))
    );
    // Into the next words: to the end of the word under the mouse.
    setup.drag(0, 9, now);
    assert_eq!(
        setup.selected(),
        Some(((0, 4), (0, 12), Selection::Characters))
    );
    // Back before it: from the word's end to the start of that word.
    setup.drag(0, 1, now);
    assert_eq!(
        setup.selected(),
        Some(((0, 6), (0, 0), Selection::Characters))
    );
    assert_eq!(setup.release().as_deref(), Some("one two"));
}

#[test]
fn a_double_click_takes_the_files_word_characters() {
    let mut setup = Setup::new("a margin-top b\n");
    setup
        .copy
        .update(clackatoo_protocol::BufferUpdate::Language { word: "-".into() });
    setup.editor.settle(&setup.copy);
    setup.press(0, 4, 2);
    assert_eq!(
        setup.selected(),
        Some(((0, 2), (0, 11), Selection::Characters))
    );
}

#[test]
fn a_triple_click_selects_the_line_and_its_drag_whole_lines() {
    let mut setup = Setup::new("one\ntwo\nthree\nfour\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(1, 1, 3);
    assert_eq!(setup.selected(), Some(((1, 1), (1, 1), Selection::Lines)));
    setup.drag(2, 0, now);
    assert_eq!(setup.selected(), Some(((1, 1), (2, 0), Selection::Lines)));
    assert_eq!(setup.release().as_deref(), Some("two\nthree\n"));
}

#[test]
fn an_operator_after_letting_go_acts_on_the_selection() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    setup.press(0, 6, 1);
    setup.drag(1, 1, Instant::now());
    setup.release();
    setup.keys("d");
    assert_eq!(setup.text(), "hello cond line\n");
    assert_eq!(setup.editor.mode, Mode::Normal);
    // And by lines, `>`.
    setup.press(0, 0, 3);
    setup.release();
    setup.keys(">");
    assert_eq!(setup.text(), "    hello cond line\n");
}

#[test]
fn a_drag_over_an_inlay_hint_takes_the_character_its_drawn_before() {
    let mut setup = Setup::with_hints();
    setup.editor.settle(&setup.copy);
    // Line numbers are four cells: `let total: i32 = add(a: left, ...`.
    setup.press(0, 0, 1);
    // On `: i32`, drawn before the blank after `total`.
    setup.drag(0, 11, Instant::now());
    assert_eq!(
        setup.selected(),
        Some(((0, 0), (0, 9), Selection::Characters))
    );
    // On `a: `, the `l` of `left` it's drawn before.
    setup.drag(0, 22, Instant::now());
    assert_eq!(
        setup.selected(),
        Some(((0, 0), (0, 16), Selection::Characters))
    );
    // What's copied is the text, not the hints.
    assert_eq!(setup.release().as_deref(), Some("let total = add(l"));
}

#[test]
fn past_the_bottom_the_view_scrolls_while_held_faster_further_and_stops_at_the_end() {
    let text = numbered(100);
    let mut setup = Setup::new(&text);
    setup.editor.settle(&setup.copy);
    let start = Instant::now();
    setup.press(2, 1, 1);
    // Within the view, nothing wakes the window.
    setup.drag(5, 1, start);
    assert_eq!(setup.editor.drag_wakes(), None);
    // Just past the bottom: a row at once, the selection to the edge's.
    assert!(setup.drag(10, 1, start));
    assert_eq!(setup.editor.top.line, 1);
    assert_eq!(
        setup.selected(),
        Some(((2, 1), (10, 1), Selection::Characters))
    );
    let wake = setup.editor.drag_wakes().expect("a wake for the next row");
    assert!(wake > start, "never at once");
    // Nothing more until then.
    assert!(
        !setup
            .editor
            .tick_drag(start + Duration::from_millis(1), &mut setup.copy)
    );
    assert!(setup.editor.tick_drag(wake, &mut setup.copy));
    assert_eq!(setup.editor.top.line, 2);
    assert_eq!(setup.cursor(), (11, 1));
    // Further past, sooner.
    let near = setup.editor.drag_wakes().unwrap() - wake;
    setup.drag(20, 1, wake);
    let far = setup.editor.drag_wakes().unwrap() - wake;
    assert!(far < near, "{far:?} < {near:?}");
    // A long wait makes up a tenth of a second at most: no jump.
    let later = wake + Duration::from_secs(10);
    assert!(setup.editor.tick_drag(later, &mut setup.copy));
    assert!(setup.editor.top.line < 2 + 30, "{}", setup.editor.top.line);
    // Back in the view, it stops.
    setup.drag(4, 1, later);
    assert_eq!(setup.editor.drag_wakes(), None);
    // On to the end: the last line comes to the bottom, and no further,
    // and nothing wakes the window then.
    let mut now = later;
    setup.drag(12, 1, now);
    for _ in 0..1000 {
        let Some(wake) = setup.editor.drag_wakes() else {
            break;
        };
        now = wake;
        setup.editor.tick_drag(now, &mut setup.copy);
    }
    assert_eq!(setup.editor.drag_wakes(), None);
    assert_eq!(setup.editor.top.line, 90, "the last line at the bottom");
    assert_eq!(setup.cursor(), (99, 1));
    // Letting go stops it all.
    setup.drag(-2, 1, now);
    assert!(setup.editor.drag_wakes().is_some(), "up now");
    setup.release();
    assert_eq!(setup.editor.drag_wakes(), None);
    assert!(setup.selected().is_some());
}

#[test]
fn past_the_top_the_view_scrolls_up_to_the_files_start() {
    let text = numbered(50);
    let mut setup = Setup::new(&text);
    setup.keys("30Gzt");
    setup.editor.settle(&setup.copy);
    let top = setup.editor.top.line;
    let mut now = Instant::now();
    setup.press(3, 0, 1);
    assert!(setup.drag(-1, 0, now));
    assert_eq!(setup.editor.top.line, top - 1);
    while let Some(wake) = setup.editor.drag_wakes() {
        now = wake;
        setup.editor.tick_drag(now, &mut setup.copy);
    }
    assert_eq!(setup.editor.top.line, 0);
    assert_eq!(
        setup.selected(),
        Some(((top + 3, 0), (0, 0), Selection::Characters))
    );
}

#[test]
fn the_view_keeps_still_while_held_and_scrolloff_comes_back_on_letting_go() {
    let text = numbered(40);
    let mut setup = Setup::new(&text);
    setup.editor.settle(&setup.copy);
    // On the bottom row, within `scrolloff` of the edge: the view doesn't
    // move under the mouse while it's held.
    setup.press(5, 0, 1);
    setup.drag(9, 0, Instant::now());
    assert_eq!(setup.editor.top.line, 0);
    assert_eq!(setup.cursor(), (9, 0));
    // An edit from elsewhere meanwhile doesn't move it either.
    let update = setup.daemon.agent(0, "new\n");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(update);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    assert_eq!(setup.editor.top.line, 0);
    setup.release();
    assert!(setup.editor.top.line > 0, "scrolloff, once let go");
}

#[test]
fn a_drag_from_insert_mode_selects_and_goes_back_to_insert_mode_after() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    setup.keys("A!");
    assert_eq!(setup.editor.mode, Mode::Insert);
    let now = Instant::now();
    // A click stays in insert mode.
    setup.press(0, 1, 1);
    assert_eq!(setup.editor.mode, Mode::Insert);
    setup.drag(0, 6, now);
    assert_eq!(
        setup.selected(),
        Some(((0, 1), (0, 6), Selection::Characters))
    );
    setup.release();
    // `y` copies it, and goes back to insert mode, at its start.
    setup.keys("y");
    assert_eq!(setup.register().text, "ello w");
    assert_eq!(setup.editor.mode, Mode::Insert);
    setup.keys("X");
    assert_eq!(setup.text(), "hXello world!\nsecond line\n");

    // Esc too goes back to insert mode, where the cursor is; Esc again
    // to normal mode.
    setup.press(1, 0, 1);
    setup.drag(1, 3, now);
    setup.release();
    setup.keys("<Esc>");
    assert_eq!(setup.editor.mode, Mode::Insert);
    setup.keys("Y<Esc>");
    assert_eq!(setup.text(), "hXello world!\nsecYond line\n");
    assert_eq!(setup.editor.mode, Mode::Normal);

    // `c` types over it, in insert mode as it was.
    setup.keys("i");
    setup.press(1, 0, 2);
    setup.release();
    setup.keys("c2nd<Esc>");
    assert_eq!(setup.text(), "hXello world!\n2nd line\n");
    assert_eq!(setup.editor.mode, Mode::Normal);

    // A click on it goes back to insert mode, at the click.
    setup.keys("i");
    setup.press(0, 0, 3);
    setup.release();
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Lines));
    setup.press(1, 1, 1);
    assert_eq!(setup.editor.mode, Mode::Insert);
    assert_eq!(setup.cursor(), (1, 1));
}

#[test]
fn code_actions_asked_for_a_selection_made_from_insert_mode_are_picked_in_normal_mode() {
    let mut setup = Setup::new("hello world\n");
    setup.editor.settle(&setup.copy);
    setup.keys("i");
    setup.press(0, 0, 2);
    setup.release();
    setup.keys("gra");
    assert_eq!(setup.editor.mode, Mode::Normal, "for the menu's keys");
    assert!(!setup.queries.is_empty(), "asked");
}

#[test]
fn a_selection_made_from_insert_mode_leaves_it_in_normal_mode_when_the_focus_goes() {
    let mut setup = Setup::new("hello\n");
    setup.editor.settle(&setup.copy);
    setup.keys("i");
    setup.press(0, 0, 2);
    setup.release();
    setup.editor.leave();
    assert_eq!(setup.editor.mode, Mode::Normal);
    // Nor does a later key go back to it.
    setup.keys("h");
    assert_eq!(setup.editor.mode, Mode::Normal);
    assert_eq!(setup.cursor(), (0, 3));
}

#[test]
fn in_a_narrow_pane_lines_that_dont_wrap_are_hit_where_shown() {
    let mut setup = Setup::new(&format!("{}\n", "abcdefghij".repeat(10)));
    setup.set(|options| options.wrap = false);
    setup.editor.resize(Size::new(20, 5).unwrap());
    // Scrolled across, as following the cursor along the line leaves it.
    setup.editor.settle(&setup.copy);
    let left = 30;
    setup.editor.left = left;
    setup.press(0, 0, 1);
    setup.drag(0, 3, Instant::now());
    let at = |col: usize| Point { line: 0, col };
    assert_eq!(setup.editor.anchor, Some(at(left)));
    assert_eq!(setup.editor.cursor.at, at(left + 3));
}

#[test]
fn a_double_click_by_the_edge_takes_the_word_though_the_first_scrolled_the_view() {
    let text = numbered(40);
    let mut setup = Setup::new(&text);
    setup.editor.settle(&setup.copy);
    // The first click, on the bottom row, scrolls `scrolloff` rows in as
    // it's let go.
    setup.press(9, 6, 1);
    setup.release();
    assert!(setup.editor.top.line > 0);
    setup.press(9, 6, 2);
    assert_eq!(
        setup.selected(),
        Some(((9, 5), (9, 5), Selection::Characters))
    );
    setup.release();
    setup.press(9, 6, 3);
    assert_eq!(setup.selected(), Some(((9, 5), (9, 5), Selection::Lines)));
    setup.release();
    // A key, and the next click is wherever it is.
    setup.keys("<Esc>");
    let top = setup.editor.top.line;
    setup.press(5, 0, 1);
    assert_eq!(setup.cursor(), (top + 5, 0));
}

impl Setup {
    /// An agent puts `text` at byte `at`, as the daemon says.
    fn agent_puts(&mut self, at: usize, text: &str) {
        let update = self.daemon.agent(at, text);
        self.editor.hold(&self.copy);
        let mut heard = self.copy.update(update);
        self.editor.heard(&mut heard, false, &mut self.copy);
    }
}

#[test]
fn an_edit_from_elsewhere_mid_drag_moves_where_it_started_with_the_text() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(1, 2, 1);
    setup.drag(1, 5, now);
    setup.agent_puts(0, "new\n");
    // The text the drag started on is a line down now.
    setup.drag(2, 8, now);
    assert_eq!(
        setup.selected(),
        Some(((2, 2), (2, 8), Selection::Characters))
    );
    assert_eq!(setup.release().as_deref(), Some("cond li"));
    setup.keys("d");
    assert_eq!(setup.text(), "new\nhello world\nsene\n");

    // By words, both ends of the word clicked move.
    setup.press(1, 7, 1);
    setup.release();
    setup.press(1, 7, 2);
    setup.agent_puts(0, "x\n");
    setup.drag(3, 0, now);
    assert_eq!(
        setup.selected(),
        Some(((2, 6), (3, 3), Selection::Characters))
    );
}

#[test]
fn a_double_click_after_an_edit_from_elsewhere_takes_the_word_first_clicked() {
    let mut setup = Setup::new("one two\nthree four\n");
    setup.editor.settle(&setup.copy);
    setup.press(1, 7, 1);
    setup.release();
    setup.agent_puts(0, "zero\n");
    setup.press(1, 7, 2);
    assert_eq!(
        setup.selected(),
        Some(((2, 6), (2, 9), Selection::Characters))
    );
}

#[test]
fn a_key_typed_mid_drag_ends_it() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(0, 0, 1);
    setup.drag(0, 4, now);
    setup.keys("<Esc>");
    assert!(!setup.drag(1, 4, now), "the mouse no longer selects");
    assert_eq!(setup.selected(), None);
    assert_eq!(setup.release(), None);
}

#[test]
fn a_selection_from_insert_mode_ended_by_a_command_leaves_no_insert_mode_behind() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    setup.keys("A!");
    setup.press(0, 0, 2);
    setup.release();
    // Done to it as the config's Lua or a binding does, not by a key, and
    // not through the desk, which would go back to insert mode.
    setup
        .editor
        .ex("s/hello/howdy/", &mut setup.copy, &mut setup.shared, false)
        .unwrap();
    setup.serve();
    assert_eq!(setup.editor.mode, Mode::Normal);
    // The next key is normal mode's, and stays so.
    setup.keys("jdd");
    assert_eq!(setup.text(), "howdy world!\n");
    assert_eq!(setup.editor.mode, Mode::Normal);
}

#[test]
fn undo_after_a_drag_from_insert_mode_takes_back_the_delete_then_the_typing() {
    let mut setup = Setup::new("hello world\n");
    setup.editor.settle(&setup.copy);
    setup.keys("A!");
    setup.press(0, 0, 1);
    setup.drag(0, 5, Instant::now());
    setup.release();
    setup.keys("d");
    assert_eq!(setup.text(), "world!\n");
    assert_eq!(setup.editor.mode, Mode::Insert);
    setup.keys("<Esc>u");
    assert_eq!(setup.text(), "hello world!\n");
    setup.keys("u");
    assert_eq!(setup.text(), "hello world\n");
    // `.` deletes as much again, from the cursor, staying in normal mode:
    // the selection it makes isn't one from insert mode.
    setup.keys("0.");
    assert_eq!(setup.text(), "world\n");
    assert_eq!(setup.editor.mode, Mode::Normal);
}

#[test]
fn a_double_click_in_an_empty_file_has_nothing_for_the_primary_selection() {
    let mut setup = Setup::new("");
    setup.editor.settle(&setup.copy);
    setup.press(0, 0, 2);
    assert_eq!(setup.release(), None);
}

#[test]
fn the_text_replaced_mid_drag_ends_it() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(0, 0, 1);
    setup.drag(0, 4, now);
    let replaced = super::super::Daemon::new("other text\n").snapshot();
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(replaced);
    assert_eq!(heard, super::super::Heard::Replaced);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    assert!(!setup.drag(0, 8, now), "the mouse no longer selects");
    assert_eq!(setup.release(), None);
}

#[test]
fn gv_selects_what_was_dragged_again() {
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    let now = Instant::now();
    setup.press(0, 2, 1);
    setup.drag(1, 3, now);
    setup.release();
    // A click ends it, and `gv` brings it back.
    setup.press(1, 8, 1);
    assert_eq!(setup.selected(), None);
    setup.keys("gv");
    assert_eq!(
        setup.selected(),
        Some(((0, 2), (1, 3), Selection::Characters))
    );
    // Whole lines, by a triple click, ended by a drag that starts anew:
    // in visual mode, `gv` swaps between the two.
    setup.keys("<Esc>");
    setup.press(1, 0, 3);
    setup.release();
    setup.press(0, 0, 1);
    setup.drag(0, 4, now);
    setup.release();
    setup.keys("gv");
    assert_eq!(setup.selected(), Some(((1, 0), (1, 0), Selection::Lines)));
    setup.keys("gv");
    assert_eq!(
        setup.selected(),
        Some(((0, 0), (0, 4), Selection::Characters))
    );
}

#[test]
fn gv_swaps_a_double_or_triple_click_made_in_visual_mode_with_what_it_ended() {
    let word = ((1, 7), (1, 10), Selection::Characters);
    // Each press of a double or triple click comes, and goes, in turn.
    for (clicks, clicked) in [(2, word), (3, ((1, 8), (1, 8), Selection::Lines))] {
        let mut setup = Setup::new("hello world\nsecond line\n");
        setup.editor.settle(&setup.copy);
        setup.keys("wve");
        for click in 1..=clicks {
            setup.press(1, 8, click);
            setup.release();
        }
        assert_eq!(setup.selected(), Some(clicked), "{clicks}");
        setup.keys("gv");
        assert_eq!(
            setup.selected(),
            Some(((0, 6), (0, 10), Selection::Characters)),
            "{clicks}"
        );
        setup.keys("gv");
        assert_eq!(setup.selected(), Some(clicked), "{clicks}");
    }
    // A drag, then a press soon after where it began, which the window
    // counts as a double click, made in the visual mode the drag left.
    let mut setup = Setup::new("hello world\nsecond line\n");
    setup.editor.settle(&setup.copy);
    setup.press(1, 8, 1);
    setup.drag(0, 2, Instant::now());
    setup.release();
    let dragged = setup.selected();
    assert!(dragged.is_some());
    setup.press(1, 8, 2);
    setup.release();
    assert_eq!(setup.selected(), Some(word));
    setup.keys("gv");
    assert_eq!(setup.selected(), dragged);
}

#[test]
fn dot_changes_as_much_as_was_dragged_over_typing_the_same() {
    let mut setup = Setup::new("hello world\nsecond line\nthird row\n");
    setup.editor.settle(&setup.copy);
    setup.press(0, 0, 1);
    setup.drag(0, 4, Instant::now());
    setup.release();
    setup.keys("cbye<Esc>j0.");
    assert_eq!(setup.text(), "bye world\nbyed line\nthird row\n");
    // From insert mode too: `c` types on in it, and `.` makes it all again.
    setup.keys("ji");
    setup.press(2, 0, 2);
    setup.release();
    setup.keys("cfourth<Esc>");
    assert_eq!(setup.text(), "bye world\nbyed line\nfourth row\n");
    setup.keys("gg0.");
    assert_eq!(setup.text(), "fourthorld\nbyed line\nfourth row\n");
}

#[test]
fn the_column_clicked_is_the_one_wanted_after_the_wheel_moves_the_cursor() {
    let text = format!("abcdefghijklmnop\n{}", "ab\n".repeat(30));
    let mut setup = Setup::new(&text);
    setup.set(|options| {
        options.scrolloff = 0;
        options.startofline = false;
    });
    setup.editor.settle(&setup.copy);
    setup.press(0, 10, 1);
    setup.release();
    assert_eq!(setup.cursor(), (0, 10));
    // The wheel takes the cursor down onto a short line; gg goes back to
    // the column clicked, as Vim's.
    setup.editor.wheel(-5, &setup.copy);
    assert_ne!(setup.cursor().0, 0);
    setup.keys("gg");
    assert_eq!(setup.cursor(), (0, 10));
}

#[test]
fn a_click_aims_for_the_column_clicked_not_the_characters() {
    // Past the line's end, in a tab, or on a wide character's right half:
    // `j` goes on to that column, as Vim's `jump_to_mouse()` sets
    // `curswant` to it.
    for (text, col, went) in [
        ("ab\nabcdefghijkl\n", 8, (1, 8)),
        ("\tx\nabcdefghijkl\n", 1, (1, 1)),
        ("中\nabcdefghijkl\n", 1, (1, 1)),
    ] {
        let mut setup = Setup::new(text);
        setup.editor.settle(&setup.copy);
        setup.press(0, col, 1);
        setup.release();
        setup.keys("j");
        assert_eq!(setup.cursor(), went, "{text:?} at {col}");
    }
}
