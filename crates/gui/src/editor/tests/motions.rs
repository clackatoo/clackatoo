//! Motions with counts, moved by and taken by operators, each case checked
//! against Neovim 0.11 headless (`crates/gui/tests/nvim/check.lua`).
//!
//! A count before `gg` or `G` is a line number, as a move (`5gg`, `3G`)
//! and as an operator's target (`d3G`, `d3gg`, `y2gg`), from above or
//! below it, multiplied by a count before the operator (`2d3gg` is line
//! 6), and the last line for a count past it (`99G`, `d99G`). `gg` and
//! `G`, with a count or without, keep the column, as Neovim's do, or with
//! `startofline` go to the line's first non-blank, as Vim's do.

use super::Setup;
use crate::motion::{Cursor, Point};

/// Eight lines, every other one indented, so a first non-blank shows.
const EIGHT: &str = "one\n  two\nthree\n  four\nfive\n  six\nseven\n  eight\n";

/// The text, the cursor, and what `p` puts, after `keys` from the cursor
/// put where `from` puts it.
fn after(from: &str, keys: &str) -> (String, (u64, usize), String, bool) {
    let mut setup = Setup::new(EIGHT);
    setup.keys(from);
    setup.keys(keys);
    let register = setup.register();
    (setup.text(), setup.cursor(), register.text, register.lines)
}

#[test]
fn a_count_before_gg_or_g_goes_to_that_line() {
    // From the last line's fifth column, as near it as each line goes.
    assert_eq!(after("G4l", "5gg").1, (4, 3), "from below");
    assert_eq!(after("2l", "3G").1, (2, 2), "from above");
    assert_eq!(after("G4l", "1G").1, (0, 2));
    // A count past the end is the last line.
    assert_eq!(after("", "99G").1, (7, 0));
    assert_eq!(after("", "99gg").1, (7, 0));
}

#[test]
fn a_count_before_gg_or_g_as_a_target_takes_the_lines_to_that_line() {
    // From above.
    let (text, cursor, held, lines) = after("gg", "d3G");
    assert_eq!(text, "  four\nfive\n  six\nseven\n  eight\n");
    assert_eq!(held, "one\n  two\nthree\n");
    assert!(lines);
    assert_eq!(cursor.0, 0);
    // `.` takes as many again.
    assert_eq!(after("gg", "d3G.").0, "seven\n  eight\n");
    // From below, by either.
    for keys in ["d3gg", "d3G"] {
        let (text, cursor, held, _) = after("G", keys);
        assert_eq!(text, "one\n  two\n", "{keys}");
        assert_eq!(held, "three\n  four\nfive\n  six\nseven\n  eight\n");
        assert_eq!(cursor.0, 1, "{keys}");
    }
    // Line 1, the same as `gg`; and without a count, `G` is the last.
    let (text, _, held, _) = after("4G", "d1G");
    assert_eq!(text, "five\n  six\nseven\n  eight\n");
    assert_eq!(held, "one\n  two\nthree\n  four\n");
    assert_eq!(after("4G", "dgg").0, text);
    assert_eq!(after("4G", "dG").0, "one\n  two\nthree\n");
    // Past the end, to the last line.
    let (text, cursor, ..) = after("3G", "d99G");
    assert_eq!(text, "one\n  two\n");
    assert_eq!(cursor.0, 1);
}

#[test]
fn a_count_before_the_operator_multiplies_the_line_number() {
    let (text, cursor, held, _) = after("gg", "2d2G");
    assert_eq!(text, "five\n  six\nseven\n  eight\n");
    assert_eq!(held, "one\n  two\nthree\n  four\n");
    assert_eq!(cursor.0, 0);
    let (text, cursor, held, _) = after("G", "2d3gg");
    assert_eq!(text, "one\n  two\nthree\n  four\nfive\n");
    assert_eq!(held, "  six\nseven\n  eight\n");
    assert_eq!(cursor.0, 4);
}

#[test]
fn y2gg_copies_from_line_two_and_p_puts_it_there() {
    let (text, cursor, held, lines) = after("G", "y2gg");
    assert_eq!(text, EIGHT, "nothing changed");
    assert_eq!(held, "  two\nthree\n  four\nfive\n  six\nseven\n  eight\n");
    assert!(lines);
    assert_eq!(cursor.0, 1, "at the first line copied");
    let (text, cursor, ..) = after("G", "y2ggP");
    let copied = &EIGHT["one\n".len()..];
    assert_eq!(text, format!("one\n{copied}{copied}"));
    assert_eq!(cursor, (1, 2));
    // From above, and uncounted, as before.
    let (_, cursor, held, _) = after("6G", "y3G");
    assert_eq!(held, "three\n  four\nfive\n  six\n");
    assert_eq!(cursor.0, 2);
    assert_eq!(after("4G", "ygg").2, "one\n  two\nthree\n  four\n");
}

#[test]
fn a_counted_gg_in_visual_mode_selects_to_that_line() {
    let (text, cursor, held, _) = after("5G", "V2ggd");
    assert_eq!(text, "one\n  six\nseven\n  eight\n");
    assert_eq!(held, "  two\nthree\n  four\nfive\n");
    assert_eq!(cursor.0, 1);
}

#[test]
fn gg_and_g_keep_the_column_unless_startofline() {
    // And the column wanted stays, for `j` after.
    assert_eq!(after("G4l", "ggj").1, (1, 4));
    assert_eq!(after("2l", "G").1, (7, 2));
    // With `startofline`, at the first non-blank.
    let sol = |from: &str, keys: &str| {
        let mut setup = Setup::new(EIGHT);
        setup.set(|options| options.startofline = true);
        setup.keys(from);
        setup.keys(keys);
        setup.cursor()
    };
    assert_eq!(sol("G4l", "gg"), (0, 0));
    // From column 0, which keeping it would keep to.
    assert_eq!(sol("", "6G"), (5, 2));
    assert_eq!(sol("", "G"), (7, 2));
    assert_eq!(sol("", "6gg"), (5, 2));
    assert_eq!(after("", "6G").1, (5, 0), "and without it");
}

#[test]
fn a_line_number_after_colon_keeps_the_column_unless_startofline() {
    let after = |from: &str, line: &str, startofline: bool| {
        let mut setup = Setup::new(EIGHT);
        setup.set(|options| options.startofline = startofline);
        setup.keys(from);
        let done = setup
            .editor
            .ex(line, &mut setup.copy, &mut setup.shared, false);
        assert!(done.is_ok(), "{line}");
        setup.cursor()
    };
    assert_eq!(after("G4l", "5", false), (4, 3));
    assert_eq!(after("", "6", false), (5, 0));
    assert_eq!(after("", "6", true), (5, 2));
    // The column wanted, after `$` the line's end.
    assert_eq!(after("$", "7", false), (6, 4));
}

/// The text and the cursor after `keys` from where `from` puts it, with
/// `startofline` as `sol` says.
fn lines_after(from: &str, keys: &str, sol: bool) -> (String, (u64, usize)) {
    let mut setup = Setup::new(EIGHT);
    setup.set(|options| options.startofline = sol);
    setup.keys(from);
    setup.keys(keys);
    (setup.text(), setup.cursor())
}

#[test]
fn linewise_deletes_and_shifts_keep_the_column_unless_startofline() {
    // `dd` on `three`: `  four` comes up under the cursor.
    let gone = "one\n  two\n  four\nfive\n  six\nseven\n  eight\n";
    assert_eq!(lines_after("3G4l", "dd", false), (gone.to_owned(), (2, 4)));
    assert_eq!(lines_after("3G4l", "dd", true).1, (2, 2));
    // As far as the line goes.
    assert_eq!(lines_after("3G4l", "dj", false).1, (2, 3));
    assert_eq!(lines_after("3G4l", "dj", true).1, (2, 0));
    assert_eq!(lines_after("5G3l", "dk", false).1, (3, 3));
    assert_eq!(lines_after("5G3l", "dk", true).1, (3, 2));
    assert_eq!(lines_after("5G3l", "dG", false).1, (3, 3));
    assert_eq!(lines_after("5G3l", "dG", true).1, (3, 2));
    assert_eq!(lines_after("3G2l", ">>", false).1, (2, 2));
    assert_eq!(lines_after("3G2l", ">>", true).1, (2, 4));
    assert_eq!(lines_after("3G2l", ">j", false).1, (2, 2));
    assert_eq!(lines_after("3G2l", ">j", true).1, (2, 4));
    assert_eq!(lines_after("4G4l", "<<", false).1, (3, 3));
    assert_eq!(lines_after("4G4l", "<<", true).1, (3, 0));
}

#[test]
fn colon_d_goes_to_the_first_non_blank_whatever_startofline_says() {
    let after = |from: &str, line: &str, startofline: bool| {
        let mut setup = Setup::new(EIGHT);
        setup.set(|options| options.startofline = startofline);
        setup.keys(from);
        let done = setup
            .editor
            .ex(line, &mut setup.copy, &mut setup.shared, false);
        assert!(done.is_ok(), "{line}");
        setup.cursor()
    };
    for sol in [false, true] {
        // `  four` comes up to line 3, `five` to line 4.
        assert_eq!(after("", "3d", sol), (2, 2), "sol {sol}");
        assert_eq!(after("4G4l", "d", sol), (3, 0), "sol {sol}");
    }
}

#[test]
fn yanks_keep_the_column_whatever_startofline_says_but_gg_doesnt() {
    assert_eq!(lines_after("3G4l", "yj", true).1, (2, 4));
    assert_eq!(lines_after("3G4l", "yk", true).1, (1, 4));
    assert_eq!(lines_after("3G4l", "yy", true).1, (2, 4));
    // `gg` goes to the first non-blank itself.
    assert_eq!(lines_after("4G3l", "ygg", true).1, (0, 0));
    // As does `'a`, back to the mark's line, startofline or not.
    assert_eq!(lines_after("2G4l", "ma3jy'a", false).1, (1, 2));
}

#[test]
fn a_count_finds_that_many_and_semicolon_after_t_goes_on() {
    let at = |keys: &str| {
        let mut setup = Setup::new("ab.cd.ef.gh\n");
        setup.keys(keys);
        (setup.text(), setup.cursor().1)
    };
    let line = "ab.cd.ef.gh\n".to_owned();
    // Before the second `.`, not stuck before the first.
    assert_eq!(at("2t."), (line.clone(), 4));
    // Not that many: nowhere, nor anything deleted.
    assert_eq!(at("4f."), (line.clone(), 0));
    assert_eq!(at("d4f."), (line, 0));
    assert_eq!(at("d2t.").0, ".ef.gh\n");
    // `;` after `t` goes past the `.` it stopped before, but a count
    // counts it.
    assert_eq!(at("t.;").1, 4);
    assert_eq!(at("t.2;").1, 4);
    assert_eq!(at("$T.;").1, 6);
    assert_eq!(at("$2T.").1, 6);
}

/// Indents, blanks after, an empty line and punctuation, as check.lua's
/// `spaced`.
const SPACED: &str = "  one two.three\nfour  five\n\n  six seven  \neight\n";

/// The text, the cursor, with lines counted from 1 as check.lua counts
/// them, and what `p` puts, after `keys` from line `line` and byte `col`
/// of `SPACED`.
fn spaced(line: u64, col: usize, keys: &str) -> (String, (u64, usize), String) {
    let mut setup = Setup::new(SPACED);
    setup.keys(&format!("{line}G0"));
    for _ in 0..col {
        setup.keys("l");
    }
    setup.keys(keys);
    let (at_line, at_col) = setup.cursor();
    (setup.text(), (at_line + 1, at_col), setup.register().text)
}

/// Just the cursor of [`spaced`].
fn cursor_after(line: u64, col: usize, keys: &str) -> (u64, usize) {
    spaced(line, col, keys).1
}

#[test]
fn plus_enter_minus_and_underscore_go_to_lines_first_non_blanks() {
    assert_eq!(cursor_after(1, 5, "+"), (2, 0));
    assert_eq!(cursor_after(1, 5, "<CR>"), (2, 0));
    assert_eq!(cursor_after(1, 5, "3+"), (4, 2));
    assert_eq!(cursor_after(1, 5, "9+"), (5, 0));
    assert_eq!(cursor_after(5, 2, "+"), (5, 2), "no line after");
    assert_eq!(cursor_after(2, 3, "-"), (1, 2));
    assert_eq!(cursor_after(5, 3, "3-"), (2, 0));
    assert_eq!(cursor_after(1, 8, "_"), (1, 2));
    assert_eq!(cursor_after(2, 8, "3_"), (4, 2));
    assert_eq!(cursor_after(2, 8, "9_"), (5, 0));
}

#[test]
fn plus_minus_and_underscore_take_whole_lines() {
    let lines = "  one two.three\nfour  five\n";
    let rest = "\n  six seven  \neight\n";
    assert_eq!(spaced(1, 5, "d+"), (rest.into(), (1, 0), lines.into()));
    assert_eq!(spaced(2, 3, "y-").2, lines);
    assert_eq!(spaced(1, 8, "d2_"), (rest.into(), (1, 0), lines.into()));
    // A count past the last line takes nothing.
    assert_eq!(spaced(5, 2, "d2_"), (SPACED.into(), (5, 2), String::new()));
}

#[test]
fn g_underscore_goes_to_the_last_non_blank() {
    assert_eq!(cursor_after(4, 0, "g_"), (4, 10));
    assert_eq!(cursor_after(3, 0, "2g_"), (4, 10));
    assert_eq!(cursor_after(3, 0, "g_"), (3, 0));
    let (text, at, taken) = spaced(4, 2, "dg_");
    assert_eq!(
        (text.lines().nth(3), at, taken.as_str()),
        (Some("    "), (4, 2), "six seven")
    );
}

#[test]
fn bar_goes_to_the_column_a_count_says_and_wants_it() {
    assert_eq!(cursor_after(1, 12, "5|"), (1, 4));
    assert_eq!(cursor_after(1, 2, "30|"), (1, 14));
    assert_eq!(cursor_after(1, 12, "|"), (1, 0));
    assert_eq!(cursor_after(1, 2, "30|j"), (2, 9));
    let (text, at, taken) = spaced(1, 12, "d5|");
    assert_eq!(
        (text.lines().next(), at, taken.as_str()),
        (Some("  onree"), (1, 4), "e two.th")
    );
}

#[test]
fn ge_goes_back_to_the_end_of_the_word_before() {
    assert_eq!(cursor_after(1, 6, "ge"), (1, 4));
    assert_eq!(cursor_after(1, 8, "ge"), (1, 4));
    assert_eq!(cursor_after(2, 1, "ge"), (1, 14), "over a line's end");
    assert_eq!(
        cursor_after(4, 3, "ge"),
        (3, 0),
        "stopping at an empty line"
    );
    assert_eq!(cursor_after(3, 0, "ge"), (2, 9));
    assert_eq!(cursor_after(2, 6, "3ge"), (1, 9));
    assert_eq!(cursor_after(1, 14, "gE"), (1, 4));
    assert_eq!(cursor_after(1, 0, "ge"), (1, 0));
    assert_eq!(cursor_after(1, 3, "ge"), (1, 0));
    // Taking both ends.
    let (text, at, taken) = spaced(1, 6, "dge");
    assert_eq!(
        (text.lines().next(), at, taken.as_str()),
        (Some("  onwo.three"), (1, 4), "e t")
    );
    let (text, at, taken) = spaced(2, 6, "dgE");
    assert_eq!(
        (text.lines().nth(1), at, taken.as_str()),
        (Some("fouive"), (2, 3), "r  f")
    );
}

#[test]
fn space_and_backspace_go_on_over_line_ends() {
    assert_eq!(cursor_after(1, 13, "3<Space>"), (2, 1));
    assert_eq!(cursor_after(2, 9, "2<Space>"), (4, 0), "over an empty line");
    assert_eq!(cursor_after(5, 4, "<Space>"), (5, 4), "none after the last");
    assert_eq!(cursor_after(2, 0, "<BS>"), (1, 14));
    assert_eq!(cursor_after(4, 0, "<BS>"), (3, 0));
    assert_eq!(cursor_after(2, 1, "3<BS>"), (1, 13));
}

#[test]
fn space_takes_a_lines_end_as_a_character_and_d_backspace_the_line_break() {
    let line = |n: usize, (text, at, taken): (String, (u64, usize), String)| {
        (text.lines().nth(n).map(str::to_owned), at, taken)
    };
    let fiv = Some("four  fiv".to_owned());
    // On a line's last character, Space takes it, and a count past the
    // line's end ends there, as Vim's does, until it's past the next.
    assert_eq!(
        line(1, spaced(2, 9, "d<Space>")),
        (fiv.clone(), (2, 8), "e".into())
    );
    assert_eq!(
        line(1, spaced(2, 9, "d2<Space>")),
        (fiv.clone(), (2, 8), "e".into())
    );
    assert_eq!(
        line(1, spaced(2, 9, "d3<Space>")),
        (fiv, (2, 8), "e\n".into())
    );
    let joined = Some("four  fiv six seven  ".to_owned());
    assert_eq!(
        line(1, spaced(2, 9, "d4<Space>")),
        (joined, (2, 9), "e\n\n ".into())
    );
    assert_eq!(
        line(4, spaced(5, 2, "d9<Space>")),
        (Some("ei".into()), (5, 1), "ght".into())
    );
    // Back from a line's start, `d` takes the line break; `y` the
    // character before it.
    let joined_one = Some("  one two.threefour  five".to_owned());
    assert_eq!(
        line(0, spaced(2, 0, "d<BS>")),
        (joined_one, (1, 15), "\n".into())
    );
    let joined_two = Some("  one two.threfour  five".to_owned());
    assert_eq!(
        line(0, spaced(2, 0, "d2<BS>")),
        (joined_two, (1, 14), "e\n".into())
    );
    let joined_three = Some("  one two.threour  five".to_owned());
    assert_eq!(
        line(0, spaced(2, 1, "d3<BS>")),
        (joined_three, (1, 14), "e\nf".into())
    );
    assert_eq!(spaced(2, 1, "y3<BS>").1, (1, 13));
    assert_eq!(spaced(2, 1, "y3<BS>").2, "ee\nf");
    assert_eq!(spaced(2, 0, "y<BS>").2, "e");
}

/// The text and the cursor, its column in cells, after `keys` from cell
/// `col` of line `line` (from 1) of `text`, with tabs four cells wide.
fn in_cells(text: &str, (line, col): (u64, usize), keys: &str) -> (String, (u64, usize)) {
    let mut setup = Setup::new(text);
    setup.set(|options| options.tabstop = 4);
    setup.editor.cursor = Cursor::new(Point {
        line: line - 1,
        col,
    });
    setup.keys(keys);
    let (at_line, at_col) = setup.cursor();
    (setup.text(), (at_line + 1, at_col))
}

#[test]
fn a_tab_is_one_character_to_every_motion() {
    let text = "\tx\ty.z\n";
    // Space and Backspace step over a tab whole, as h and l do.
    assert_eq!(in_cells(text, (1, 0), "<Space>").1, (1, 4));
    assert_eq!(in_cells(text, (1, 4), "2<Space>").1, (1, 8));
    assert_eq!(in_cells("\t\tq\n", (1, 8), "4<BS>").1, (1, 0));
    assert_eq!(in_cells("a\tb\tc\n", (1, 0), "3l").1, (1, 5));
    assert_eq!(in_cells("a\tb\tc\n", (1, 8), "3h").1, (1, 1));
    assert_eq!(in_cells(text, (1, 0), "d2<Space>").0, "\ty.z\n");
    assert_eq!(in_cells(text, (1, 10), "gU9<BS>").0, "\tX\tY.z\n");
    // A tab's cells are a tab to find, not blanks.
    assert_eq!(in_cells("a\tb c\n", (1, 0), "f<Tab>").1, (1, 1));
    assert_eq!(in_cells("a\tb c\n", (1, 0), "f ").1, (1, 5));
    assert_eq!(in_cells("a\tb\tc\td\n", (1, 0), "t<Tab>;").1, (1, 4));
    assert_eq!(in_cells("a\tb\tc\n", (1, 0), "df<Tab>").0, "b\tc\n");
}

#[test]
fn semicolon_after_t_goes_past_a_character_not_a_cell() {
    // Wide characters take two cells each.
    assert_eq!(in_cells("中x中x中y\n", (1, 0), "tx;").1, (1, 3));
    assert_eq!(in_cells("中x中x中y\n", (1, 0), "$T中;").1, (1, 5));
}

#[test]
fn the_cursor_goes_onto_blanks_at_a_lines_end_as_vims_does() {
    let text = "a  b  \none\n";
    assert_eq!(in_cells(text, (1, 0), "$").1, (1, 5));
    assert_eq!(in_cells(text, (1, 3), "3l").1, (1, 5));
    assert_eq!(in_cells(text, (1, 3), "3<Space>").1, (2, 0));
    assert_eq!(in_cells(text, (2, 0), "<BS>").1, (1, 5));
    // Backspace from the next line's start takes just the line break,
    // and Space the blanks before it.
    assert_eq!(in_cells(text, (2, 0), "d<BS>").0, "a  b  one\n");
    assert_eq!(in_cells(text, (1, 3), "d2<Space>").0, "a   \none\n");
    assert_eq!(in_cells(text, (1, 3), "d3<Space>").0, "a  \none\n");
}

#[test]
fn t_right_before_its_character_takes_the_one_under_the_cursor() {
    assert_eq!(in_cells("ab)c\n", (1, 1), "dt)"), ("a)c\n".into(), (1, 1)));
    assert_eq!(
        in_cells("ab)c)d\n", (1, 1), "dt)."),
        ("a)d\n".into(), (1, 1))
    );
    assert_eq!(in_cells("ab)c\n", (1, 1), "ct)X<Esc>").0, "aX)c\n");
    // With a count, there must be that many.
    assert_eq!(in_cells("ab)c\n", (1, 1), "d2t)").0, "ab)c\n");
    // Moving, it stays, and doesn't fail.
    let mut setup = Setup::new("ab)c\n");
    setup.keys("lt)");
    assert_eq!(setup.cursor(), (0, 1));
    assert!(!setup.editor.take_failed());
}

#[test]
fn d_over_lines_from_the_indent_to_a_lines_end_takes_them_whole() {
    let text = "abc\n  de\nfg hi\n";
    // Vim's d: over lines, from at or before the first non-blank, to the
    // end of a line, whole lines.
    assert_eq!(in_cells(text, (1, 0), "d2e").0, "fg hi\n");
    assert_eq!(in_cells(text, (1, 0), "d2g_").0, "fg hi\n");
    assert_eq!(in_cells(text, (1, 0), "d2w").0, "fg hi\n");
    assert_eq!(in_cells("hello\nworld x\nx\n", (1, 0), "d2e").0, " x\nx\n");
    // dw stops at the end of the line its last word is on.
    assert_eq!(in_cells("a b\n  de\n", (1, 2), "d2w").0, "a \n");
    assert_eq!(in_cells("abc\nde fg\n", (1, 0), "d2w").0, "fg\n");
    // An empty line is a word.
    assert_eq!(in_cells("abc\n\nx\n", (1, 0), "d2w").0, "x\n");
}

#[test]
fn backspace_onto_an_empty_line_takes_it_as_an_exclusive_motion_does() {
    let text = "abc\n\nx y\n";
    assert_eq!(in_cells(text, (3, 0), "c<BS>Z<Esc>").0, "abc\nZ\nx y\n");
    assert_eq!(in_cells(text, (3, 0), "d<BS>").0, "abc\nx y\n");
}

#[test]
fn operators_over_lines_leave_the_cursor_where_vims_do() {
    let text = "one\n  two\nthree\n";
    // gU goes back to where the motion or the object starts.
    assert_eq!(in_cells(text, (2, 4), "gU-").1, (1, 0));
    assert_eq!(in_cells(text, (2, 4), "gU_").1, (2, 2));
    assert_eq!(in_cells("one\n  two\n\nx\n", (2, 4), "gUip").1, (1, 0));
    // y_ leaves it where it is.
    assert_eq!(in_cells(text, (2, 4), "y_").1, (2, 4));
    // 2_ fails from the last line.
    let mut setup = Setup::new("ab\ncd\n");
    setup.keys("j2_");
    assert!(setup.editor.take_failed());
}

/// The text after `keys` from `at`, as [`in_cells`] has it, and what the
/// register holds, and whether as whole lines.
fn kept(text: &str, (line, col): (u64, usize), keys: &str) -> (String, String, bool) {
    let mut setup = Setup::new(text);
    setup.set(|options| options.tabstop = 4);
    setup.editor.cursor = Cursor::new(Point {
        line: line - 1,
        col,
    });
    setup.keys(keys);
    let register = setup.register();
    (setup.text(), register.text, register.lines)
}

#[test]
fn w_taken_stops_at_the_end_of_its_last_words_line() {
    let chars = |text: &str| (text.to_string(), false);
    let taken = |text, at, keys| {
        let (text, register, lines) = kept(text, at, keys);
        (text, (register, lines))
    };
    // As Vim's: the line break stays, whatever the next line starts with.
    assert_eq!(
        taken("foo\nbar\n", (1, 0), "dw"),
        ("\nbar\n".into(), chars("foo"))
    );
    assert_eq!(taken("  \nend\n", (1, 0), "yw").1, chars("  "));
    assert_eq!(
        taken("foo  \nbar\n", (1, 0), "dw"),
        ("\nbar\n".into(), chars("foo  "))
    );
    assert_eq!(
        taken("  x\nbar\n", (1, 2), "dw"),
        ("  \nbar\n".into(), chars("x"))
    );
    assert_eq!(
        taken("a b\nc\n", (1, 0), "d2w"),
        ("\nc\n".into(), chars("a b"))
    );
    assert_eq!(
        taken("foo\n\nbar\n", (1, 0), "dw"),
        ("\n\nbar\n".into(), chars("foo"))
    );
    assert_eq!(
        taken("foo\n  bar\n", (1, 0), "dW"),
        ("\n  bar\n".into(), chars("foo"))
    );
    // At the text's end, to the end of its blanks.
    assert_eq!(
        taken("x foo  ", (1, 2), "dw"),
        ("x ".into(), chars("foo  "))
    );
    // Over lines before the last, on as `w` goes.
    assert_eq!(taken("foo\nbar baz\n", (1, 0), "d2w").0, "baz\n");
    assert_eq!(taken("foo\nbar baz\n", (1, 0), "d3w").0, "");
}

#[test]
fn a_motion_by_characters_that_takes_whole_lines_leaves_d_at_the_first_non_blank() {
    // `dw` on an empty line takes it whole, and the cursor goes to the
    // next line's first non-blank, `startofline` or not, as Vim's.
    assert_eq!(
        in_cells("\n  bar\n", (1, 0), "dw"),
        ("  bar\n".into(), (1, 2))
    );
    assert_eq!(in_cells("  ab\n\n    bar\n", (2, 0), "dw").1, (2, 4));
}

#[test]
fn a_lines_end_is_past_its_last_character_however_wide() {
    // d and Backspace at a line's start join it to the line before, which
    // ends in a wide character or a tab.
    assert_eq!(
        in_cells("x中\ny\n", (2, 0), "d<BS>"),
        ("x中y\n".into(), (1, 3))
    );
    assert_eq!(
        in_cells("x\t\ny\n", (2, 0), "d<BS>"),
        ("x\ty\n".into(), (1, 4))
    );
    assert_eq!(
        in_cells("x\t\ny\n", (2, 0), "d2<BS>"),
        ("xy\n".into(), (1, 1))
    );
    assert_eq!(in_cells("x中\ny\n", (2, 0), "c<BS>Z<Esc>").0, "x中Zy\n");
    // w taken stops past it.
    assert_eq!(kept("a 中\ny\n", (1, 2), "dw").1, "中");
    assert_eq!(kept("a\t\ny\n", (1, 0), "yw").1, "a\t");
}

#[test]
fn on_a_tab_up_and_down_aim_for_its_last_cell() {
    // Vim's cursor sits on a tab's last cell, and aims for it.
    let below = "\tfoo\nabcdefgh\n";
    assert_eq!(in_cells(below, (1, 0), "j").1, (2, 3));
    assert_eq!(in_cells("a\tb\nabcdef\n", (1, 0), "lj").1, (2, 3));
    assert_eq!(in_cells(below, (1, 0), "0j").1, (2, 3));
    assert_eq!(in_cells(below, (1, 0), "vj").1, (2, 3));
    assert_eq!(
        in_cells("abcdefgh\nx\tab\nabcdefgh\n", (2, 0), "xj").1,
        (3, 3)
    );
    assert_eq!(
        in_cells("abcdefgh\n\tx\nabcdefgh\n", (2, 0), "ddk").1,
        (1, 3)
    );
    // But on its first in visual mode before the other end, as Vim's
    // cursor is drawn there.
    assert_eq!(in_cells(below, (1, 5), "vhhj").1, (2, 0));
    // A wide character's first.
    assert_eq!(in_cells("中文\nabcdef\n", (1, 0), "lj").1, (2, 2));
    // Pulled back from past a line's end after `2x` or `d$`, it aims for
    // where it is.
    assert_eq!(in_cells("abc\ncd\n", (1, 1), "2xj").1, (2, 0));
    assert_eq!(in_cells("abc\ncd\n", (1, 1), "d$j").1, (2, 0));
    // Passing a tab, the column wanted stays.
    assert_eq!(in_cells("abcd\n\tx\nabcdefgh\n", (1, 0), "jj").1, (3, 0));
    // Past a line's end in visual mode, as a motion can leave Vim's
    // cursor, it aims for there.
    let text = "  \n  indented line.\na\t(b)\tc\n";
    assert_eq!(in_cells(text, (3, 5), "V3ed").1, (2, 9));
}

#[test]
fn b_stops_at_empty_lines_as_w_does() {
    let text = "  - item\n\n\n";
    assert_eq!(in_cells(text, (3, 0), "b").1, (2, 0));
    assert_eq!(in_cells(text, (3, 0), "2B").1, (1, 4));
    assert_eq!(in_cells("ab\n\n\ncd\n", (4, 0), "3b").1, (1, 0));
    // A line of blanks isn't empty.
    assert_eq!(in_cells("ab\n  \ncd\n", (3, 0), "b").1, (1, 0));
}

#[test]
fn w_at_the_texts_end_goes_to_its_last_character_blank_or_not() {
    assert_eq!(in_cells("ab\n  \n", (1, 0), "W").1, (2, 1));
    assert_eq!(in_cells("ab  \n", (1, 0), "w").1, (1, 3));
    assert_eq!(in_cells("ab\n\t\n", (1, 0), "W").1, (2, 0));
    // Past it in visual mode, as Vim's.
    assert_eq!(kept("ab  \n", (1, 0), "vwy").1, "ab  ");
}

#[test]
fn a_line_of_blanks_first_non_blank_is_its_last_blank() {
    assert_eq!(in_cells("  \nx\n", (2, 0), "k_").1, (1, 1));
    assert_eq!(in_cells("  \nx\n", (2, 0), "-").1, (1, 1));
    assert_eq!(in_cells("x\n  \n", (1, 0), "+").1, (2, 1));
    assert_eq!(in_cells("\t\t\n", (1, 0), "^").1, (1, 4));
    // Still in its indent, for d, and gU, which takes the line whole and
    // leaves the cursor where it was.
    assert_eq!(in_cells("  \nab\n", (1, 1), "d2w").0, "");
    assert_eq!(in_cells("a\t(b)\tc\n  \n\n", (2, 1), "5gUW").1, (2, 1));
}

#[test]
fn i_on_a_line_of_blanks_types_after_them_and_selected_at_the_start() {
    assert_eq!(in_cells("  \nx\n", (1, 0), "Iy<Esc>").0, "  y\nx\n");
    assert_eq!(in_cells("\t\nx\n", (1, 0), "Iy<Esc>").0, "\ty\nx\n");
    // Selected by characters or lines, at the start of the other end's
    // line, or the cursor if it's on a line before.
    assert_eq!(in_cells("  abc\nx\n", (1, 3), "vIy<Esc>").0, "y  abc\nx\n");
    assert_eq!(in_cells("foo  \n", (1, 4), "vFoIy<Esc>").0, "yfoo  \n");
    assert_eq!(
        in_cells("  abc\n  de\n", (1, 3), "vjIy<Esc>").0,
        "y  abc\n  de\n"
    );
    assert_eq!(
        in_cells("  abc\nde\n", (2, 1), "vkIy<Esc>").0,
        " y abc\nde\n"
    );
}

#[test]
fn dl_short_of_its_count_at_a_lines_end_takes_the_last_character() {
    assert_eq!(in_cells("x\n", (1, 0), "dl").0, "\n");
    assert_eq!(in_cells("foo\n", (1, 0), "d4l").0, "\n");
    assert_eq!(in_cells("foo\n", (1, 0), "d2l").0, "o\n");
    assert_eq!(in_cells("x\n", (1, 0), "clZ<Esc>").0, "Z\n");
    assert_eq!(kept("a\t\n", (1, 0), "y5l").1, "a\t");
    // An empty line has none.
    assert_eq!(in_cells("\nx\n", (1, 0), "dl").0, "\nx\n");
}

#[test]
fn a_count_takes_dollar_to_a_line_further_down() {
    let text = "ab cd\nef gh\nij\n";
    assert_eq!(in_cells(text, (1, 0), "2$").1, (2, 4));
    assert_eq!(in_cells(text, (1, 0), "5$").1, (3, 1));
    // From the last line there's none to go to, but it aims for lines'
    // ends all the same.
    assert_eq!(in_cells(text, (3, 0), "2$").1, (3, 0));
    assert_eq!(in_cells("a中b\tc\n\t\n", (2, 0), "V2$d").1, (1, 8));
    // Over lines from a line's start, d takes them whole, as `2D` does.
    assert_eq!(
        kept(text, (1, 0), "d2$"),
        ("ij\n".into(), "ab cd\nef gh\n".into(), true)
    );
    assert_eq!(kept(text, (1, 0), "2D").0, "ij\n");
    assert_eq!(kept(text, (1, 2), "d2$").0, "ab\nij\n");
    assert_eq!(kept(text, (1, 0), "V2$y").1, "ab cd\nef gh\n");
}

#[test]
fn j_on_the_last_line_and_k_on_the_first_go_nowhere() {
    // Not even along the line to the column wanted, as Vim's.
    assert_eq!(in_cells("ab cd\nef gh\nij\n", (3, 0), "2$j").1, (3, 0));
    assert_eq!(in_cells("abc\nxy\n", (1, 0), "$jk").1, (1, 2));
    let mut setup = Setup::new("ab cd\nef gh\nij\n");
    setup.keys("G2$");
    setup.keys("j");
    assert_eq!(setup.cursor(), (2, 0));
    assert!(setup.editor.take_failed());
    // Nor past the end in visual mode.
    assert_eq!(in_cells("abc\nxy\n", (1, 0), "$vk").1, (1, 2));
}

#[test]
fn a_count_takes_percent_to_a_line_so_far_through() {
    let text = "a\nb\nc\nd\ne\n";
    assert_eq!(in_cells(text, (1, 0), "50%").1, (3, 0));
    assert_eq!(in_cells(text, (1, 0), "5%").1, (1, 0));
    assert_eq!(in_cells(text, (1, 0), "100%").1, (5, 0));
    // Nowhere past 100.
    assert_eq!(in_cells(text, (3, 0), "101%").1, (3, 0));
    // Whole lines.
    assert_eq!(
        kept(text, (1, 0), "d50%"),
        ("d\ne\n".into(), "a\nb\nc\n".into(), true)
    );
    // Aiming for the column wanted, as `gg` does with `nostartofline`.
    assert_eq!(in_cells("abc\nb\nc\nd\ne\n", (1, 0), "$40%").1, (2, 0));
    assert_eq!(in_cells("abc\n  bcd\nc\nd\ne\n", (1, 2), "40%").1, (2, 2));
}

#[test]
fn a_count_of_paragraphs_short_of_them_goes_nowhere() {
    let text = "a\n\nb\n\nc\n";
    assert_eq!(in_cells(text, (1, 0), "5}").1, (1, 0));
    assert_eq!(in_cells(text, (5, 0), "5{").1, (5, 0));
    // The last may run into the text's end.
    assert_eq!(in_cells(text, (1, 0), "3}").1, (5, 0));
    // Taking nothing.
    assert_eq!(in_cells(text, (1, 0), "d5}").0, text);
}

#[test]
fn a_motion_that_fails_keeps_the_column_wanted_where_vims_does() {
    // h and l that can't go keep it, as Vim's `oneleft` and `oneright`.
    assert_eq!(in_cells("ab\n\t\nabcdefgh\n", (1, 0), "jlj").1, (3, 0));
    assert_eq!(in_cells("ab\nx\t\nabcdefgh\n", (1, 0), "j$lj").1, (3, 7));
    assert_eq!(in_cells("abcdef\nab\n", (1, 5), "jlk").1, (1, 5));
    // A count of paragraphs short of them, as words that can't go, aim
    // for where the cursor is.
    assert_eq!(in_cells("abcdef\nxy\n\nz\n", (1, 5), "j3}k").1, (1, 1));
    assert_eq!(in_cells("abcdef\nx\n", (1, 5), "jek").1, (1, 0));
}
