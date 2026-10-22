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
