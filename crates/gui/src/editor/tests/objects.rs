//! Text objects that go by lines and sentences, `ip` and `ap`, each case
//! checked against Neovim 0.11 headless (`crates/gui/tests/nvim/check.lua`).

use super::Setup;
use crate::vim::{Mode, Selection};

/// Two paragraphs, two blank lines, two more, as check.lua's `para`.
const PARA: &str = "one\n  two\n\n\nthree\nfour\n\nfive\n";

/// The text, the cursor (lines counted from 1, as check.lua counts them),
/// and what `p` puts and whether it's whole lines, after `keys` from line
/// `line` and byte `col` of `text`.
fn after(
    text: &str,
    (line, col): (u64, usize),
    keys: &str,
) -> (String, (u64, usize), String, bool) {
    let mut setup = Setup::new(text);
    setup.keys(&format!("{line}G0"));
    for _ in 0..col {
        setup.keys("l");
    }
    setup.keys(keys);
    let (at_line, at_col) = setup.cursor();
    let register = setup.register();
    (
        setup.text(),
        (at_line + 1, at_col),
        register.text,
        register.lines,
    )
}

/// What `keys` copies from `at` in `PARA`, and where the cursor goes.
fn copied(at: (u64, usize), keys: &str) -> (String, (u64, usize)) {
    let (_, cursor, taken, lines) = after(PARA, at, keys);
    assert!(lines || taken.is_empty(), "{keys}: whole lines");
    (taken, cursor)
}

#[test]
fn ip_and_ap_take_a_paragraph_and_ap_its_blank_lines() {
    let first = "one\n  two\n".to_owned();
    assert_eq!(copied((2, 3), "yip"), (first.clone(), (1, 0)));
    assert_eq!(copied((2, 3), "yap"), (first + "\n\n", (1, 0)));
    // On blank lines, `ip` is them, and `ap` the paragraph after too.
    assert_eq!(copied((3, 0), "yip"), ("\n\n".into(), (3, 0)));
    assert_eq!(copied((3, 0), "yap"), ("\n\nthree\nfour\n".into(), (3, 0)));
}

#[test]
fn a_count_takes_that_many_and_the_blank_lines_count_for_ip() {
    assert_eq!(copied((1, 0), "2yip").0, "one\n  two\n\n\n");
    assert_eq!(copied((1, 0), "2yap").0, "one\n  two\n\n\nthree\nfour\n\n");
    // Past the last paragraph, `ap` takes the blank lines before.
    assert_eq!(
        copied((5, 2), "y2ap"),
        ("\n\nthree\nfour\n\nfive\n".into(), (3, 0))
    );
    // Not that many: nothing.
    assert_eq!(copied((8, 0), "3yip"), (String::new(), (8, 0)));
}

#[test]
fn deleting_a_paragraph_leaves_the_cursor_in_its_column() {
    assert_eq!(
        after(PARA, (6, 1), "dip"),
        (
            "one\n  two\n\n\n\nfive\n".into(),
            (5, 0),
            "three\nfour\n".into(),
            true
        )
    );
    assert_eq!(
        after(PARA, (6, 1), "dap"),
        (
            "one\n  two\n\n\nfive\n".into(),
            (5, 1),
            "three\nfour\n\n".into(),
            true
        )
    );
    // The last, with no blank lines after it, takes those before.
    assert_eq!(
        after(PARA, (8, 2), "dap"),
        (
            "one\n  two\n\n\nthree\nfour\n".into(),
            (6, 2),
            "\nfive\n".into(),
            true
        )
    );
    assert_eq!(
        after(PARA, (2, 3), "cipx<Esc>").0,
        "x\n\n\nthree\nfour\n\nfive\n"
    );
}

#[test]
fn vip_selects_whole_lines() {
    let mut setup = Setup::new(PARA);
    setup.keys("2G3lvip");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Lines));
    assert_eq!(setup.cursor(), (1, 0));
    assert_eq!(copied((2, 3), "vipy"), ("one\n  two\n".into(), (1, 0)));
}

#[test]
fn a_line_of_blanks_ends_a_paragraph_as_an_empty_one_does() {
    let text = "one\n  \ntwo\n\t\n\nthree\n";
    assert_eq!(after(text, (1, 0), "yip").2, "one\n");
    assert_eq!(after(text, (1, 0), "dap").0, "two\n\t\n\nthree\n");
}

/// What `y` copies after `keys` from line `line`, column 0, of `PARA`.
fn selected(line: u64, keys: &str) -> String {
    after(PARA, (line, 0), &format!("{keys}y")).2
}

#[test]
fn ip_and_ap_take_a_selection_on_by_paragraphs() {
    let two = "one\n  two\n\n\n";
    assert_eq!(selected(1, "vipip"), two);
    assert_eq!(selected(1, "vip2ip"), "one\n  two\n\n\nthree\nfour\n");
    assert_eq!(selected(1, "vapap"), "one\n  two\n\n\nthree\nfour\n\n");
    // By characters, it stays so, taking the empty line's break.
    assert_eq!(selected(1, "vjip"), two);
    assert_eq!(selected(1, "Vjip"), two);
    // Back, from the second paragraph's last line.
    assert_eq!(selected(6, "vkap"), "one\n  two\n\n\nthree\nf");
    let mut setup = Setup::new(PARA);
    setup.keys("vjip");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Characters));
}
