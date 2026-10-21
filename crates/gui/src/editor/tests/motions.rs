//! Motions with counts, moved by and taken by operators, each case checked
//! against Neovim 0.11 headless (`crates/gui/tests/nvim/check.lua`).
//!
//! A count before `gg` or `G` is a line number, as a move (`5gg`, `3G`)
//! and as an operator's target (`d3G`, `d3gg`, `y2gg`), from above or
//! below it, multiplied by a count before the operator (`2d3gg` is line
//! 6), and the last line for a count past it (`99G`, `d99G`). Where
//! clackatoo differs on purpose: a counted `gg` or `G` goes to the line's
//! first non-blank, as Vim does with `startofline`, where Neovim keeps the
//! column.

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
    let mut setup = Setup::new(EIGHT);
    setup.keys("G");
    setup.keys("5gg");
    assert_eq!(setup.cursor(), (4, 0), "from below");
    setup.keys("gg");
    setup.keys("3G");
    assert_eq!(setup.cursor(), (2, 0), "from above");
    setup.keys("G1G");
    assert_eq!(setup.cursor(), (0, 0));
    setup.keys("6gg");
    assert_eq!(setup.cursor(), (5, 2), "to its first non-blank");
    // A count past the end is the last line.
    setup.keys("gg99G");
    assert_eq!(setup.cursor(), (7, 2));
    setup.keys("gg99gg");
    assert_eq!(setup.cursor(), (7, 2));
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
