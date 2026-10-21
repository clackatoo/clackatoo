//! Long lines wrapped over several screen rows: how they're drawn, moved
//! through, scrolled and clicked, and everything drawn over them.

use clackatoo_protocol::{BufferUpdate, Diagnostic, Severity, Size};
use clackatoo_term::Underline;

use super::{Answer, Setup};
use crate::motion::{Point, RowAt};
use crate::render::HighlightKind;

/// The rows drawn, as text.
fn drawn(setup: &mut Setup) -> Vec<String> {
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    rows.iter().map(|row| row.text()).collect()
}

/// Where the cursor's drawn, as a row and column of the pane.
fn cursor_drawn(setup: &mut Setup) -> Option<(usize, usize)> {
    setup.editor.settle(&setup.copy);
    let (_, cursor) = setup.editor.scene();
    cursor.map(|cursor| (cursor.line, cursor.col))
}

/// The view's first row.
fn top(setup: &Setup) -> RowAt {
    setup.editor.top.row(&setup.editor.text(&setup.copy))
}

/// A pane `cols` wide and `rows` high: right of the four cells of line
/// numbers, `cols - 4` cells for text.
fn sized(text: &str, cols: u16, rows: u16) -> Setup {
    let mut setup = Setup::new(text);
    setup.editor.resize(Size::new(cols, rows).unwrap());
    setup
}

#[test]
fn long_lines_go_on_over_rows_with_no_number_and_wide_characters_whole() {
    // 15 cells for text.
    let mut setup = sized(
        "abcdefghijklmnopqrstuvwxyz\nabcdefghijklmn字x\nabcdefghijklm\tx\n",
        19,
        7,
    );
    assert_eq!(
        drawn(&mut setup),
        [
            "  1 abcdefghijklmno",
            "    pqrstuvwxyz",
            // 字 doesn't split: it goes on to the next row.
            "  1 abcdefghijklmn",
            "    字x",
            // The tab, from cell 13 to 16, goes on over by a cell.
            "  2 abcdefghijklm",
            "     x",
            "~",
        ]
    );
    // nowrap scrolls sideways instead.
    setup.set(|options| options.wrap = false);
    assert_eq!(
        drawn(&mut setup)[..2],
        ["  1 abcdefghijklmno", "  1 abcdefghijklmn"]
    );
    setup.keys("$");
    assert_eq!(drawn(&mut setup)[0], "  1 lmnopqrstuvwxyz");
    // And back, the view's no longer sideways.
    setup.set(|options| options.wrap = true);
    assert_eq!(drawn(&mut setup)[0], "  1 abcdefghijklmno");
}

#[test]
fn linebreak_breaks_between_words() {
    let mut setup = sized("the quick brown fox jumps over\n", 19, 4);
    assert_eq!(
        drawn(&mut setup),
        ["  1 the quick", "    brown fox", "    jumps over", "~"]
    );
    setup.set(|options| options.linebreak = false);
    assert_eq!(
        drawn(&mut setup),
        ["  1 the quick brown", "     fox jumps over", "~", "~"]
    );
}

#[test]
fn breakindent_and_showbreak_start_the_rows_a_line_goes_on_over() {
    let line = format!("    {}", "word ".repeat(10));
    let mut setup = Setup::new(&format!("{}\n", line.trim_end()));
    assert_eq!(
        drawn(&mut setup)[..2],
        [
            "  1     word word word word word word",
            "        word word word word"
        ]
    );
    setup.set(|options| options.showbreak = "↪ ".into());
    let rows = drawn(&mut setup);
    assert_eq!(rows[1], "        ↪ word word word word");
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    let fg = rows[1].style(rows[1].cells()[8]).fg;
    assert_eq!(fg, crate::colors::line_number(false).fg, "showbreak, dim");
    setup.set(|options| options.breakindent = false);
    assert_eq!(drawn(&mut setup)[1], "    ↪ word word word word");
}

#[test]
fn j_and_k_move_by_line_and_gj_and_gk_by_row() {
    // Rows of 36: the first line goes on over three, from 0, 36 and 72.
    let text = format!("{}\nshort\n{}\n", "a".repeat(80), "b".repeat(50));
    let mut setup = Setup::new(&text);
    setup.keys("5l");
    setup.keys("gj");
    assert_eq!(setup.cursor(), (0, 41));
    setup.keys("gj");
    assert_eq!(setup.cursor(), (0, 77));
    setup.keys("gj");
    assert_eq!(setup.cursor(), (1, 4), "as far along as the line goes");
    setup.keys("gj");
    assert_eq!(setup.cursor(), (2, 5), "the column wanted, kept");
    setup.keys("gkgk");
    assert_eq!(setup.cursor(), (0, 77));
    setup.keys("3gk");
    assert_eq!(setup.cursor(), (0, 5), "only as far as the first row");
    // j and k go by line.
    setup.keys("j");
    assert_eq!(setup.cursor(), (1, 4));
    setup.keys("k");
    assert_eq!(setup.cursor(), (0, 5));
    setup.keys("2j");
    assert_eq!(setup.cursor(), (2, 5));
    setup.keys("gg03gj");
    assert_eq!(setup.cursor(), (1, 0));

    // $ and 0 are the line's; g$ and g0 the row's.
    setup.keys("gg$");
    assert_eq!(setup.cursor(), (0, 79));
    setup.keys("g0");
    assert_eq!(setup.cursor(), (0, 72));
    setup.keys("0g$");
    assert_eq!(setup.cursor(), (0, 35));
    setup.keys("gjg0");
    assert_eq!(setup.cursor(), (0, 36));
    // gj goes by rows' ends after $.
    setup.keys("gg$gk");
    assert_eq!(setup.cursor(), (0, 71));
    // With an operator, it's not by lines: dgj deletes up to the cursor
    // a row down.
    setup.keys("gg0dgj");
    assert_eq!(setup.text().lines().next().unwrap().len(), 80 - 36);
}

#[test]
fn gj_steps_off_a_tab_begun_on_the_row_before() {
    // The tab, from cell 34 to 40 with tabstop 8, goes on over the row
    // after; gj from cell 1 lands in it, so on the character after.
    let mut setup = Setup::new(&format!("{}\tbc\n", "a".repeat(34)));
    setup.set(|options| options.tabstop = 8);
    setup.keys("lgj");
    assert_eq!(setup.cursor(), (0, 40));
    setup.keys("gk");
    assert_eq!(setup.cursor(), (0, 1));
}

#[test]
fn scrolloff_and_paging_count_screen_rows() {
    // 30 lines of three rows each, in a pane ten rows high: scrolloff
    // keeps four rows either side.
    let text = format!("{}\n", "x".repeat(80)).repeat(30);
    let mut setup = Setup::new(&text);
    setup.keys("3j");
    // The cursor is on row 9 from the top; four rows under it is row 13.
    assert_eq!(top(&setup), RowAt { line: 1, row: 1 });
    assert_eq!(cursor_drawn(&mut setup), Some((5, 4)));
    // Ctrl+D: half a page of rows, the view and the cursor.
    setup.keys("<C-d>");
    assert_eq!(top(&setup), RowAt { line: 3, row: 0 });
    assert_eq!(setup.cursor(), (4, 72), "five rows down");
    // Ctrl+U: five rows up would start the view on line 1's second row,
    // so it starts at line 2, as Vim's does, and the cursor goes three.
    setup.keys("<C-u>");
    assert_eq!(top(&setup), RowAt { line: 2, row: 0 });
    assert_eq!(setup.cursor(), (3, 72), "three rows up");
    // zt, zz and zb put the cursor's line's rows at the top, middle and
    // bottom, scrolloff allowing.
    setup.keys("0zb");
    assert_eq!(top(&setup), RowAt { line: 1, row: 1 });
    setup.keys("zt");
    assert_eq!(top(&setup), RowAt { line: 1, row: 2 });
    // The wheel scrolls by rows.
    setup.editor.wheel(-2, &setup.copy);
    assert_eq!(top(&setup), RowAt { line: 2, row: 1 });
    // As far down as it goes, the last line's last row shows at the
    // bottom, scrolloff rows below the top.
    setup.keys("G");
    assert_eq!(drawn(&mut setup)[9], "    xxxxxxxx");
    assert_eq!(top(&setup), RowAt { line: 26, row: 2 });
}

#[test]
fn a_line_taller_than_the_pane_scrolls_through_its_rows() {
    // 20 rows of 36 cells, in a pane ten high.
    let mut setup = Setup::new(&format!("{}\nend\n", "a".repeat(36 * 20)));
    setup.keys("$");
    // Its last row, with the next line under it in the margin.
    assert_eq!(top(&setup), RowAt { line: 0, row: 11 });
    assert_eq!(cursor_drawn(&mut setup), Some((8, 4 + 35)));
    setup.keys("0");
    assert_eq!(top(&setup), RowAt { line: 0, row: 0 });
    setup.keys("10gj");
    assert_eq!(setup.cursor(), (0, 360));
    assert_eq!(top(&setup), RowAt { line: 0, row: 5 });
}

#[test]
fn a_click_on_a_row_a_line_goes_on_over_lands_on_its_character() {
    let mut setup = Setup::new(&format!("{}\nshort\n", "a".repeat(80)));
    setup.editor.settle(&setup.copy);
    setup.editor.click(1, 4 + 3, &mut setup.copy);
    assert_eq!(setup.cursor(), (0, 39));
    // On the blank numbers: the row's start.
    setup.editor.click(2, 1, &mut setup.copy);
    assert_eq!(setup.cursor(), (0, 72));
    // Past the end of the line's last row: its last character.
    setup.editor.click(2, 30, &mut setup.copy);
    assert_eq!(setup.cursor(), (0, 79));
    setup.editor.click(3, 6, &mut setup.copy);
    assert_eq!(setup.cursor(), (1, 2));

    // Past a row that ends early, at a word: its last character.
    let mut setup = sized("the quick brown fox\n", 19, 4);
    setup.editor.settle(&setup.copy);
    setup.editor.click(0, 18, &mut setup.copy);
    assert_eq!(setup.cursor(), (0, 9));
    // In the indent, the row's first character.
    let mut setup = Setup::new(&format!("    {}\n", "b".repeat(40)));
    setup.editor.settle(&setup.copy);
    setup.editor.click(1, 5, &mut setup.copy);
    assert_eq!(setup.cursor(), (0, 36));
}

#[test]
fn the_cursor_typing_at_a_full_rows_end_goes_on_the_next() {
    let mut setup = Setup::new(&format!("{}\nb\n", "a".repeat(36)));
    assert_eq!(drawn(&mut setup)[1], "  1 b", "one row");
    setup.keys("A");
    assert_eq!(cursor_drawn(&mut setup), Some((1, 4)));
    assert_eq!(drawn(&mut setup)[2], "  1 b");
    setup.keys("<Esc>");
    assert_eq!(cursor_drawn(&mut setup), Some((0, 4 + 35)));
    // Typing anywhere else on it, it's one row still.
    setup.keys("I");
    assert_eq!(drawn(&mut setup)[1], "  1 b");
    assert_eq!(cursor_drawn(&mut setup), Some((0, 4)));
}

#[test]
fn highlights_selections_and_columns_are_drawn_on_wrapped_rows() {
    let mut setup = Setup::new(&format!("{}\nshort\n", "a".repeat(80)));
    // The cursor's line, every row of it.
    assert_eq!(
        setup.highlighted(HighlightKind::CursorLine),
        [(0, 4..40), (1, 4..40), (2, 4..40)]
    );
    // A selection over a row's end goes to the pane's edge, then on.
    setup.keys("30lv10l");
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, 34..40), (1, 4..9)]
    );
    setup.keys("<Esc>Vj");
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, 4..40), (1, 4..40), (2, 4..40), (3, 4..40)]
    );
    // A column marks the cell of the line it's in: column 50 is on the
    // second row, and short lines don't reach it.
    setup.keys("<Esc>");
    setup.set(|options| options.colorcolumn = vec![10, 50]);
    assert_eq!(
        setup.highlighted(HighlightKind::ColorColumn),
        [(0, 13..14), (3, 13..14), (1, 4 + 13..4 + 14)]
    );
    // A match found over a row's end.
    let mut setup = Setup::new(&format!("{}bc\n", "a".repeat(35)));
    setup.keys("/abc<CR>");
    assert_eq!(
        setup.highlighted(HighlightKind::Match),
        [(0, 4 + 34..40), (1, 4..5)]
    );
}

#[test]
fn diagnostics_and_virtual_text_are_drawn_on_wrapped_rows() {
    let mut setup = Setup::new(&format!("{}bad\nok\n", "a ".repeat(20)));
    setup.copy.update(BufferUpdate::Diagnostics {
        version: 0,
        diagnostics: vec![Diagnostic {
            start: 40,
            end: 43,
            severity: Severity::Error,
            message: "bad".into(),
            source: None,
            code: None,
        }],
    });
    let rows = drawn(&mut setup);
    // Broken after the last blank that fits: "bad" is on the second row,
    // with what's wrong after it, and the number in its colour.
    assert_eq!(rows[1], "    a a bad  ■ bad");
    setup.keys("j");
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    let style = |row: usize, col: usize| *rows[row].style(rows[row].cells()[col]);
    assert_eq!(style(1, 4 + 4).underline, Underline::Curly);
    assert_eq!(style(0, 4).underline, Underline::None);
    let red = crate::colors::diagnostic(Severity::Error);
    assert_eq!(style(0, 2).fg, red, "the number, on the first row");
}

#[test]
fn the_panel_by_the_cursor_is_by_its_row() {
    let mut setup = Setup::new(&format!("{}\nb\n", "a".repeat(50)));
    setup.keys("40lK");
    let (asked, _, _) = setup.queries[0];
    setup.answer(
        asked,
        Answer::Hover {
            text: "a".into(),
            markdown: false,
            range: None,
        },
    );
    setup.editor.settle(&setup.copy);
    let placed = setup.editor.panel().unwrap().placed;
    // Under the second row, from the cursor's column on it.
    assert_eq!((placed.top, placed.left), (2, 4 + 4));
}

#[test]
fn gj_and_g_dollar_copy_into_registers() {
    // Rows of 36: the first line goes on over three, from 0, 36 and 72.
    // (Macros of them are the desk's: see its tests.)
    let text = format!("{}\nshort\n{}\n", "a".repeat(80), "b".repeat(50));
    let mut setup = Setup::new(&text);
    // gj with an operator and a register: what's up to a row down.
    setup.keys("gg\"cygj");
    assert_eq!(setup.held('c'), Some("a".repeat(36)));
    setup.keys("\"dyg$");
    assert_eq!(setup.held('d'), Some("a".repeat(36)));
}

#[test]
fn a_piece_the_grammar_answers_with_is_selected_on_wrapped_rows() {
    use clackatoo_protocol::{Construct, Query};

    let body = format!("two({});", "a".repeat(60));
    let text = format!("fn one(a, bb) {{\n    {body}\n}}\n");
    let mut setup = Setup::new(&text);
    setup.keys("jvif");
    let (asked, _, query) = setup.last_query();
    assert_eq!(query, Query::Pieces(Construct::Function));
    let piece = super::piece_of(&text, text.trim_end(), Some(&body));
    setup.answer(asked, Answer::Pieces(vec![piece]));
    // The body's line goes on over three rows, from cells 0, 36 and 68,
    // indented 4: it's selected on all three, and the cursor's at its end.
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(1, 8..40), (2, 8..40), (3, 8..10)]
    );
    assert_eq!(cursor_drawn(&mut setup), Some((3, 9)));
}

#[test]
fn a_line_longer_than_a_row_holds_is_drawn_and_moved_through_to_its_end() {
    // 70,000 cells: more than a terminal's row can have.
    let line = "a=b(c,d);".repeat(70_000 / 9);
    let mut setup = Setup::new(&format!("{line}\nend\n"));
    setup.keys("$");
    assert_eq!(setup.cursor(), (0, line.len() - 1));
    let rows = drawn(&mut setup);
    assert!(rows[8].ends_with(");"), "{rows:?}");
    // By rows, and back, near its end.
    setup.keys("gk");
    let (line_of, col) = setup.cursor();
    assert_eq!(line_of, 0);
    assert!(col < line.len() - 1 && col > ROW_CELLS_ABOUT, "{col}");
    setup.keys("gj");
    assert_eq!(setup.cursor(), (0, line.len() - 1));
    setup.keys("gj");
    assert_eq!(setup.cursor(), (1, 2));
}

/// Somewhere past the 65,535 cells a terminal's row can have.
const ROW_CELLS_ABOUT: usize = 65_600;

/// A line of 69,999 cells, words of four letters and a space, and a short
/// one after it, in a pane wide enough not to matter.
fn past_a_rows_end() -> (Setup, String) {
    let line = "abcd ".repeat(14_000).trim_end().to_owned();
    let text = format!("{line}\nend\n");
    (Setup::new(&text), text)
}

/// Puts the cursor on cell `col` of the first line.
fn at(setup: &mut Setup, col: usize) {
    setup.editor.go_to(0, col, &setup.copy);
    assert_eq!(setup.cursor(), (0, col));
}

#[test]
fn motions_and_objects_act_past_where_a_terminals_row_would_end() {
    // Words of four letters from every fifth cell: the last at 69,995.
    let (mut setup, text) = past_a_rows_end();
    let moved = |setup: &mut Setup, from: usize, keys: &str| {
        at(setup, from);
        setup.keys(keys);
        setup.cursor()
    };
    // f and its kin, ; and , once there's one to do again.
    assert_eq!(moved(&mut setup, 69_998, "Fa"), (0, 69_995));
    assert_eq!(moved(&mut setup, 69_998, "Tb"), (0, 69_997));
    assert_eq!(moved(&mut setup, 66_000, "fd"), (0, 66_003));
    assert_eq!(moved(&mut setup, 66_000, ";"), (0, 66_003));
    assert_eq!(moved(&mut setup, 66_000, ","), (0, 65_998));
    // Words, across the cell a terminal's row would end at, 65,535, and
    // over the last word to the next line.
    assert_eq!(moved(&mut setup, 65_530, "w"), (0, 65_535));
    assert_eq!(moved(&mut setup, 65_530, "2e"), (0, 65_538));
    assert_eq!(moved(&mut setup, 65_536, "b"), (0, 65_535));
    assert_eq!(moved(&mut setup, 65_536, "2B"), (0, 65_530));
    assert_eq!(moved(&mut setup, 69_996, "W"), (1, 0));
    assert_eq!(moved(&mut setup, 0, "jb"), (0, 69_995));
    assert_eq!(moved(&mut setup, 0, "13107w"), (0, 65_535));
    // Objects and the operators given motions.
    at(&mut setup, 65_536);
    setup.keys("diw");
    assert_eq!(setup.text().len(), text.len() - 4);
    setup.keys("u");
    at(&mut setup, 66_001);
    setup.keys("daw");
    assert_eq!(setup.text().len(), text.len() - 5);
    setup.keys("u");
    at(&mut setup, 69_990);
    setup.keys("dw");
    assert_eq!(setup.text().len(), text.len() - 5);
    setup.keys("u$db");
    assert_eq!(setup.text().len(), text.len() - 3);
    setup.keys("u");
    at(&mut setup, 69_995);
    setup.keys("cwxy<Esc>");
    assert!(setup.text().starts_with(&text[..69_995]));
    assert_eq!(&setup.text()[69_995..], "xy\nend\n");

    // Brackets and quotes far apart, on one line.
    let inside = "x".repeat(80_000);
    let mut setup = Setup::new(&format!("f({inside}) \"{inside}\"\n"));
    setup.keys("%");
    assert_eq!(setup.cursor(), (0, 80_002));
    setup.keys("%");
    assert_eq!(setup.cursor(), (0, 1));
    setup.keys("$F(di(");
    assert_eq!(setup.text(), format!("f() \"{inside}\"\n"));
    setup.keys("$di\"");
    assert_eq!(setup.text(), "f() \"\"\n");
}

#[test]
fn l_crosses_a_long_lines_row_end_as_h_does() {
    let (mut setup, _) = past_a_rows_end();
    at(&mut setup, 65_533);
    setup.keys("lll");
    assert_eq!(setup.cursor(), (0, 65_536));
    setup.keys("4h");
    assert_eq!(setup.cursor(), (0, 65_532));
    setup.keys("$l");
    assert_eq!(setup.cursor(), (0, 69_998), "no further than its end");
}

#[test]
fn a_paragraph_ending_on_a_long_line_ends_at_its_end() {
    let line = "abcd ".repeat(14_000).trim_end().to_owned();
    let mut setup = Setup::new(&format!("x\n{line}"));
    setup.keys("}");
    assert_eq!(setup.cursor(), (1, 69_998));
}

#[test]
fn without_wrap_gj_and_gk_are_j_and_k_and_g0_and_g_dollar_go_by_the_view() {
    let text = format!("{}\nb\nc\nd\n", "a".repeat(80));
    let mut setup = Setup::new(&text);
    setup.set(|options| options.wrap = false);
    // dgj takes two lines, as dj does.
    setup.keys("jdgj");
    assert_eq!(setup.text(), format!("{}\nd\n", "a".repeat(80)));
    // The first and last columns in view.
    setup.keys("gg$");
    assert_eq!(setup.editor.left, 80 - 36);
    setup.keys("g0");
    assert_eq!(setup.cursor(), (0, 44));
    setup.keys("g$");
    assert_eq!(setup.cursor(), (0, 79));
    setup.keys("0g$");
    assert_eq!(setup.cursor(), (0, 35));

    // With wrap, after $, dgj takes the character it goes to.
    let mut setup = Setup::new(&format!("{}\n{}\n", "a".repeat(10), "b".repeat(10)));
    setup.keys("$dgj");
    assert_eq!(setup.text(), "aaaaaaaaa\n");
}

#[test]
fn the_column_wanted_is_kept_to_a_row_and_kept_by_the_wheel() {
    // From column 77 onto "short", gj on to a line of rows of 36 goes as
    // far as a row goes, and j after it from there, as in Vim.
    let text = format!(
        "{}\nshort\n{}\n{}\n",
        "a".repeat(80),
        "b".repeat(200),
        "c".repeat(200)
    );
    let mut setup = Setup::new(&text);
    setup.keys("77lj");
    assert_eq!(setup.cursor(), (1, 4));
    setup.keys("gj");
    assert_eq!(setup.cursor(), (2, 35));
    setup.keys("j");
    assert_eq!(setup.cursor(), (3, 35));

    // The wheel moves the cursor into view, but the column it wants
    // stays: j goes back to it.
    let text = format!("{}\n", "x".repeat(80)).repeat(30);
    let mut setup = Setup::new(&text);
    setup.keys("70l");
    setup.editor.wheel(-20, &setup.copy);
    assert_ne!(setup.cursor().0, 0);
    setup.keys("j");
    assert_eq!(setup.cursor().1, 70);
}

#[test]
fn a_resize_keeps_the_views_text_and_the_cursor_in_view() {
    let text = format!("{}\n", "x".repeat(80)).repeat(30);
    let mut setup = Setup::new(&text);
    setup.keys("G");
    assert_eq!(top(&setup), RowAt { line: 26, row: 2 });
    // Narrower, the rows are 16 cells: the top's text, from cell 72, is
    // on row 4 of its line, and the view scrolls to keep the cursor in.
    setup.editor.resize(Size::new(20, 10).unwrap());
    assert!(cursor_drawn(&mut setup).is_some());
    assert_eq!(setup.editor.top.cell % 16, 0);
    // Wider again, it's still in view.
    setup.editor.resize(Size::new(120, 10).unwrap());
    assert!(cursor_drawn(&mut setup).is_some());
}

#[test]
fn gk_goes_onto_a_tab_that_spans_rows_and_gj_past_it() {
    // In rows of 15, a tab from cell 2 to 32 goes on over three rows,
    // with "cd" on the third.
    let mut setup = sized("ab\tcd\nx\n", 19, 6);
    setup.set(|options| options.tabstop = 32);
    setup.keys("$");
    assert_eq!(setup.cursor(), (0, 33));
    setup.keys("gk");
    assert_eq!(setup.cursor(), (0, 2), "on the tab, two rows up");
    setup.keys("gj");
    assert_eq!(setup.cursor(), (0, 32), "past it, to c");
    // A tab ending its line: gj goes on to the next.
    let mut setup = sized("ab\t\nx\n", 19, 6);
    setup.set(|options| options.tabstop = 32);
    setup.keys("$gj");
    assert_eq!(setup.cursor(), (1, 0));
}

#[test]
fn a_key_typed_into_a_long_line_ending_in_an_accented_letter_lays_it_out_right() {
    // Plain but for its last character, which is two bytes.
    let mut setup = sized(&format!("{}é\nnext\n", "a".repeat(70_000)), 24, 6);
    drawn(&mut setup);
    setup.editor.cursor.at = Point {
        line: 0,
        col: 35_000,
    };
    setup.keys("ix<Esc>$");
    assert_eq!(setup.cursor(), (0, 70_001), "on the é");
    let rows = drawn(&mut setup);
    assert!(rows.iter().any(|row| row.ends_with("aé")), "{rows:?}");
    setup.keys("x");
    assert_eq!(
        setup.text(),
        format!("{}x{}\nnext\n", "a".repeat(35_000), "a".repeat(35_000))
    );
}

#[test]
fn what_is_laid_out_follows_edits_undos_and_redos_as_laid_out_afresh() {
    use crate::editor::text::Laid;
    use crate::editor::wrap;
    use crate::motion::Lines;

    // Lines with tabs, wide characters and accents, edited in the middle,
    // split, joined, deleted, put back, undone and redone.
    let line = "\tsome words 字字 and é\u{301} more, then-some;";
    let mut setup = sized(&format!("{line}\n{line}{line}\nshort\n{line}\n"), 24, 12);
    let keys = [
        "jfdix<Esc>",
        "a\t字<Esc>",
        "i<CR><Esc>",
        "kJ",
        "u",
        "<C-r>",
        "dd",
        "p",
        "u",
        "u",
        "fwcwchanged<Esc>",
        "o\tnew 字 line<Esc>",
        "kddkP",
        "5x",
        "u",
        "ggA tail<Esc>",
        "3J",
        "u",
        "<C-r>",
    ];
    for keys in keys {
        setup.keys(keys);
        setup.editor.settle(&setup.copy);
        let wrapping = setup.editor.wrapping.at(setup.editor.text_width().max(1));
        let text = setup.editor.text(&setup.copy);
        for at in 0..text.lines() {
            let laid = text.laid(at).unwrap();
            let fresh = Laid::new(text.line_text(at), setup.editor.indent.tabstop);
            laid.assert_as(&fresh, &format!("after {keys}, line {at}"));
            if let Some(wrapping) = wrapping
                && setup.editor.mode != crate::vim::Mode::Insert
            {
                assert_eq!(
                    *text.screen_rows(at),
                    *wrap::rows_by_cells(&fresh, wrapping, false),
                    "after {keys}, line {at}"
                );
            }
        }
    }
    // A line the edits didn't touch isn't laid out again.
    let last = setup.editor.text(&setup.copy).last_line();
    let before = setup.editor.text(&setup.copy).laid(last).unwrap();
    setup.keys("ggix<CR><Esc>");
    let after = setup.editor.text(&setup.copy).laid(last + 1).unwrap();
    assert!(std::rc::Rc::ptr_eq(&before, &after));
}

/// How long a key that scrolls takes, laying out what's shown after it,
/// in a file of 100,000 lines of 150 to 450 characters, words and code
/// alike, wrapped in a pane 160 by 50. A measurement, not a check: run it
/// by hand with `cargo test -p clackatoo-gui --release scroll_times --
/// --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn scroll_times() {
    use std::time::Instant;

    let words = [
        "let",
        "value",
        "=",
        "self.buffer(index)",
        "+",
        "1;",
        "// a note",
        "if",
        "x",
    ];
    let mut text = String::new();
    for line in 0..100_000_usize {
        let mut cells = 0;
        let length = 150 + line * 37 % 300;
        let mut word = line;
        while cells < length {
            let next = words[word % words.len()];
            text.push_str(next);
            text.push(if word % 11 == 0 { '\t' } else { ' ' });
            cells += next.len() + 1;
            word += 1;
        }
        text.push('\n');
    }
    let mut setup = sized(&text, 160, 50);
    for wrap in [true, false] {
        setup.set(|options| options.wrap = wrap);
        setup.keys("gg");
        let way = if wrap { "wrap" } else { "nowrap" };
        for (name, keys, times) in [
            ("j", "j", 2000),
            ("gj", "gj", 2000),
            ("Ctrl+D", "<C-d>", 2000),
            ("Ctrl+U", "<C-u>", 2000),
            ("Page Down", "<C-f>", 2000),
            ("G then gg", "Ggg", 200),
        ] {
            let mut took: Vec<f64> = (0..times)
                .map(|_| {
                    let started = Instant::now();
                    setup.keys(keys);
                    setup.editor.settle(&setup.copy);
                    started.elapsed().as_secs_f64() * 1e6
                })
                .collect();
            took.sort_by(f64::total_cmp);
            #[allow(clippy::cast_precision_loss)]
            let mean = took.iter().sum::<f64>() / took.len() as f64;
            eprintln!(
                "{way}, {name}: least {:.1} µs, median {:.1} µs, mean {mean:.1} µs, worst {:.1} µs",
                took[0],
                took[took.len() / 2],
                took[took.len() - 1],
            );
        }
        let started = Instant::now();
        for _ in 0..2000 {
            setup.editor.wheel(-3, &setup.copy);
            setup.editor.settle(&setup.copy);
        }
        let mean = started.elapsed().as_secs_f64() * 1e6 / 2000.0;
        eprintln!("{way}, wheel: mean {mean:.1} µs");
    }
}

/// How long a frame and a key take on a 5 MB line, as a minified file
/// has, wrapped in a pane 160 by 50, near its end, and keys typed into it.
/// A measurement, not a check: run it by hand with `cargo test -p
/// clackatoo-gui --release long_line_times -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn long_line_times() {
    let line = "a=b(c,d);".repeat(5_000_000 / 9);
    let mut setup = sized(&format!("{line}\n"), 160, 50);
    time(&mut setup, "first frame", 1, &|_| {});
    time(&mut setup, "$, to the end", 1, &|setup| setup.keys("$"));
    time(&mut setup, "a frame", 10, &|_| {});
    time(&mut setup, "gk", 10, &|setup| setup.keys("gk"));
    time(&mut setup, "gj", 10, &|setup| setup.keys("gj"));
    time(&mut setup, "Ctrl+U", 10, &|setup| setup.keys("<C-u>"));
    time(&mut setup, "Ctrl+D", 10, &|setup| setup.keys("<C-d>"));
    time(&mut setup, "wheel", 10, &|setup| {
        setup.editor.wheel(3, &setup.copy);
    });
    // Keys typed at its start, in its middle and at its end.
    let cells = setup.editor.text(&setup.copy).laid(0).unwrap().cells();
    for (name, col) in [("start", 0), ("middle", cells / 2), ("end", cells)] {
        setup.keys("<Esc>");
        setup.editor.cursor.at = Point { line: 0, col };
        setup.keys(if col == cells { "a" } else { "i" });
        setup.editor.settle(&setup.copy);
        time(
            &mut setup,
            &format!("a key typed at its {name}"),
            20,
            &|setup| {
                setup.keys("x");
            },
        );
    }
    // An é typed into its middle and taken out again: it's plain again,
    // and keys typed after are as quick as before.
    setup.keys("<Esc>");
    setup.editor.cursor.at = Point {
        line: 0,
        col: cells / 2,
    };
    time(
        &mut setup,
        "an é typed in its middle and taken out",
        1,
        &|setup| {
            setup.keys("ié<BS>");
        },
    );
    time(&mut setup, "a key typed there after", 20, &|setup| {
        setup.keys("x");
    });
    // And into one that isn't plain ASCII, which keeps where each
    // character starts.
    let mut setup = sized(&format!("é{line}\n"), 160, 50);
    setup.editor.settle(&setup.copy);
    for (name, col) in [("start", 1), ("middle", cells / 2), ("end", cells + 1)] {
        setup.keys("<Esc>");
        setup.editor.cursor.at = Point { line: 0, col };
        setup.keys(if col > cells { "a" } else { "i" });
        setup.editor.settle(&setup.copy);
        let name = format!("a key typed at the {name} of one with an é");
        time(&mut setup, &name, 10, &|setup| setup.keys("x"));
    }
}

/// Prints how long `act` takes, with the frame after it, on average over
/// `times` goes, in milliseconds.
fn time(setup: &mut Setup, name: &str, times: usize, act: &dyn Fn(&mut Setup)) {
    let started = std::time::Instant::now();
    for _ in 0..times {
        act(setup);
        setup.editor.settle(&setup.copy);
    }
    #[allow(clippy::cast_precision_loss)]
    let each = started.elapsed().as_secs_f64() * 1e3 / times as f64;
    eprintln!("{name}: {each:.3} ms");
}

/// How long motions take on a 5 MB line, as a minified file has, wrapped
/// in a pane 160 by 50: word motions, `f`, `%` and `yiw` near its start
/// and in its middle, `$`, and a search for a word at its end. Then the
/// same on one that isn't plain ASCII. A measurement, not a check: run it
/// by hand with `cargo test -p clackatoo-gui --release
/// long_line_motion_times -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn long_line_motion_times() {
    let line = "a=b(c,d);".repeat(5_000_000 / 9);
    // Each with the cells before the repeats start.
    for (kind, text, before) in [
        ("plain", format!("{line} needle\n"), 0),
        ("with an é", format!("é{line} needle\n"), 1),
    ] {
        let mut setup = sized(&text, 160, 50);
        setup.editor.settle(&setup.copy);
        for (place, col) in [("near its start", 1000), ("in its middle", line.len() / 2)] {
            for (name, keys) in [
                ("w", "w"),
                ("e", "e"),
                ("b", "b"),
                ("f(", "f("),
                ("%", "%"),
                ("yiw", "yiw"),
            ] {
                setup.editor.go_to(0, col, &setup.copy);
                let name = format!("{kind}, {place}: {name}");
                time(&mut setup, &name, 20, &|setup| setup.keys(keys));
            }
        }
        // Reading far along it: to a letter at its end, quotes it has none
        // of, and brackets around an `a`, which none are, looked for as
        // far as they're looked for.
        let middle = line.len() / 2;
        let a = middle - middle % 9 + before;
        for (name, col, keys) in [
            ("fn, to its end", middle, "fn"),
            ("yi\", none", middle, "yi\""),
            ("yi(, none", a, "yi("),
        ] {
            let name = format!("{kind}, in its middle: {name}");
            time(&mut setup, &name, 5, &|setup| {
                setup.editor.go_to(0, col, &setup.copy);
                setup.keys(keys);
            });
        }
        time(&mut setup, &format!("{kind}: $"), 20, &|setup| {
            setup.keys("0$");
        });
        time(&mut setup, &format!("{kind}: / to its end"), 5, &|setup| {
            setup.keys("0/needle<CR>");
        });
    }
}

/// How long editing keys take, laying out what's shown after them, in a
/// file of 100,000 lines of 150 to 450 characters, wrapped in a pane 160
/// by 50: `o` and a word typed, a key typed at a line's end, and `dd`. A
/// measurement, not a check: run it by hand with `cargo test -p
/// clackatoo-gui --release edit_times -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn edit_times() {
    use std::time::Instant;

    let words = "let value = self.buffer(index) + 1; // a note\tif x ";
    let mut text = String::new();
    for line in 0..100_000_usize {
        text.extend(words.chars().cycle().take(150 + line * 37 % 300));
        text.push('\n');
    }
    let mut setup = sized(&text, 160, 50);
    setup.keys("50%");
    for (name, keys, times) in [
        ("j", "j", 1000),
        ("o, a word and Esc", "oword<Esc>", 500),
        ("a key typed at the end", "Ax<Esc>", 500),
        ("dd", "dd", 1000),
        ("w", "w", 2000),
        ("e", "e", 2000),
        ("b", "b", 2000),
    ] {
        let mut took: Vec<f64> = (0..times)
            .map(|_| {
                let started = Instant::now();
                setup.keys(keys);
                setup.editor.settle(&setup.copy);
                started.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        took.sort_by(f64::total_cmp);
        #[allow(clippy::cast_precision_loss)]
        let mean = took.iter().sum::<f64>() / took.len() as f64;
        eprintln!(
            "{name}: least {:.1} µs, median {:.1} µs, mean {mean:.1} µs, worst {:.1} µs",
            took[0],
            took[took.len() / 2],
            took[took.len() - 1],
        );
    }
}
