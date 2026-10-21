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
    assert_eq!(sol("2l", "6G"), (5, 2));
    assert_eq!(sol("2l", "G"), (7, 2));
}
