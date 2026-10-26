//! Text objects that go by lines, sentences and tags, `ip` and `ap`, `is`
//! and `as` with `(` and `)`, `it` and `at`, and `Y` in visual mode, each
//! case checked against Neovim 0.11 headless
//! (`crates/gui/tests/nvim/check.lua`). Neovim's text has a line more when
//! everything on its last line is deleted, an empty one; here a text's
//! last line ends with its line break, so the cursor goes to the line
//! before.

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

#[test]
fn vip_moves_the_selections_other_end_only_to_another_line() {
    let anchor = |keys: &str| {
        let mut setup = Setup::new(PARA);
        setup.keys(keys);
        setup.editor.anchor.map(|at| (at.line, at.col))
    };
    assert_eq!(anchor("2lvip"), Some((0, 2)));
    assert_eq!(anchor("j2lvip"), Some((0, 0)));
}

#[test]
fn words_as_vims_current_word_takes_them() {
    let taken = |text, at, keys| after(text, at, keys).2;
    // Blanks at a line's end, tabs too.
    assert_eq!(taken("end\t\n", (1, 0), "yaw"), "end\t");
    // A count goes on over lines.
    assert_eq!(taken("ab cd\nef gh\n", (1, 3), "y2aw"), "cd\nef ");
    assert_eq!(taken("ab cd\nef gh\n", (1, 3), "y3aw"), " cd\nef gh");
    assert_eq!(taken("one two three four\n", (1, 0), "v2awy"), "one two ");
    // In visual mode, more than a character selected takes on another,
    // away from the other end, from just past a line's end too.
    assert_eq!(taken("one two three four\n", (1, 0), "vawawy"), "one two ");
    assert_eq!(taken("one two three four\n", (1, 0), "viwiwy"), "one ");
    assert_eq!(taken("one two three four\n", (1, 8), "vhawy"), " two t");
    assert_eq!(taken("abc def\nxy\n", (1, 0), "v$awy"), "abc def\nxy");
    assert_eq!(taken("abc def\nxy\n", (1, 0), "v$iwy"), "abc def\nxy");
    // Past the text's end, it fails, leaving the cursor, or the selection,
    // where it got to.
    let (text, cursor, ..) = after("one two three\n", (1, 0), "d5aw");
    assert_eq!((text.as_str(), cursor), ("one two three\n", (1, 12)));
    assert_eq!(after("one two three\n", (1, 4), "v5aw").1, (1, 13));
}

#[test]
fn words_keep_a_block_and_end_lines_selected() {
    let mode = |keys: &str| {
        let mut setup = Setup::new("xy\nxy\n.a.a.a.\n");
        setup.keys("j");
        setup.keys(keys);
        setup.editor.mode
    };
    assert_eq!(mode("<C-v>aw"), Mode::Visual(Selection::Block));
    assert_eq!(mode("Vaw"), Mode::Visual(Selection::Characters));
}

#[test]
fn brackets_as_vims_current_block_finds_them() {
    let taken = |text, at, keys| after(text, at, keys).2;
    // None around the cursor: the next, as Neovim's.
    assert_eq!(taken("a\t(b)\tc\n", (1, 0), "ya("), "(b)");
    // A count goes out, or with none around, on into the next.
    assert_eq!(taken("f(a, (b), c)\n", (1, 6), "y2i("), "a, (b), c");
    assert_eq!(taken("x ((y)) z\n", (1, 0), "y2i("), "y");
    // Selected, from the selection's start.
    assert_eq!(taken("a\t(b)\tc\n", (1, 0), "vlla(y"), "(b)");
    // A quoted string isn't over lines: the selection stays.
    let mut setup = Setup::new("a \"b\"\nc\n");
    setup.keys("vji\"");
    assert_eq!(setup.cursor(), (1, 0));
    assert_eq!(
        setup.editor.anchor,
        Some(crate::motion::Point { line: 0, col: 0 })
    );
    assert!(setup.editor.take_failed());
}

#[test]
fn copying_an_object_after_the_cursor_goes_to_its_start() {
    let at = |text, keys| after(text, (1, 0), keys).1;
    assert_eq!(at("x \"ab\"\n", "yi\""), (1, 3));
    assert_eq!(at("x ((y)) z\n", "y2i("), (1, 4));
}

#[test]
fn d_takes_an_object_over_lines_from_the_indent_to_a_lines_end_whole() {
    let (text, _, taken, lines) = after("(a\nb)\nz\n", (1, 0), "da(");
    assert_eq!(
        (text.as_str(), taken.as_str(), lines),
        ("z\n", "(a\nb)\n", true)
    );
    assert_eq!(after("  (a\nb)\nz\n", (1, 2), "da(").0, "z\n");
    // Not from after the indent, nor to before a line's end.
    assert_eq!(after("x (a\nb)\nz\n", (1, 2), "da(").0, "x \nz\n");
    assert_eq!(after("(a\nb) y\nz\n", (1, 0), "da(").0, " y\nz\n");
    // Nor what's selected.
    assert_eq!(after("(a\nb)\nz\n", (1, 0), "va(d").0, "\nz\n");
}

#[test]
fn an_object_that_fails_aims_for_where_the_cursor_is() {
    // `$` aims for lines' ends; a( with no brackets fails, and `j` after
    // goes to the column the cursor's in, as Vim's.
    assert_eq!(after("abc\nabcdef\n", (1, 1), "v$a(j").1, (2, 3));
    assert_eq!(after("abc\nabcdef\n", (1, 1), "v$j").1, (2, 6));
    // On a tab, its last cell, as it aimed for before visual mode.
    let text = "b c\n\n\n  \nb c\n\t\n  \n";
    let mut setup = Setup::new(text);
    setup.set(|options| options.tabstop = 4);
    setup.keys("5jvapj");
    assert_eq!(setup.cursor(), (6, 2));
    // Moved there in visual mode, its first, before the other end.
    let mut setup = Setup::new("a\tb\n  \n.a.a.a.\n\tab cd\t\n\na\tb\n");
    setup.set(|options| options.tabstop = 4);
    setup.keys("4jVka(d");
    assert_eq!(setup.cursor(), (3, 0));
}

#[test]
fn more_paragraphs_than_there_are_fail_as_far_as_they_go() {
    let mut setup = Setup::new("a\n\nb\n\nc\n");
    setup.keys("Vj5ap");
    assert_eq!(setup.cursor(), (4, 0));
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Lines));
    // Which stops a macro, as Vim's.
    assert!(setup.editor.take_failed());
    setup.keys("<Esc>ggVjap");
    assert!(!setup.editor.take_failed());
}

#[test]
fn an_object_up_to_a_lines_start_is_taken_as_an_exclusive_motion() {
    // Whole lines from the indent, the cursor at the first non-blank
    // after, as Vim's.
    let (text, cursor, ..) = after("  foo\n\nbarbaz\n", (1, 2), "d2iw");
    assert_eq!((text.as_str(), cursor), ("barbaz\n", (1, 0)));
    // `i(` up to the line `)` starts: the lines between, whole.
    let (text, _, taken, lines) = after("(\n  abc\n)\nxyzw\n", (2, 3), "di(");
    assert_eq!(
        (text.as_str(), taken.as_str(), lines),
        ("(\n)\nxyzw\n", "  abc\n", true)
    );
    let (_, cursor, taken, _) = after("(\n  abc\n)\nxyzw\n", (2, 3), "yi(");
    assert_eq!((cursor, taken.as_str()), ((2, 0), "  abc\n"));
    // Selected, to just past the line's end.
    assert_eq!(after("f(a\n  x,\n)\n", (2, 2), "vi(d").0, "f()\n");
}

#[test]
fn brackets_and_quotes_selected_grow_as_vims_do() {
    let taken = |text, at, keys| after(text, at, keys).2;
    // a( again goes out a pair; a selection starting on `(` isn't in it.
    assert_eq!(taken("f(a, (b), c)\n", (1, 6), "va(a(y"), "(a, (b), c)");
    assert_eq!(taken("f(a, (b), c)\n", (1, 5), "v2li(y"), "a, (b), c");
    assert_eq!(taken("f(a, (bc), d)\n", (1, 7), "vi(i(y"), "a, (bc), d");
    let mut setup = Setup::new("x (p q) y\n");
    setup.keys("llvla(");
    assert!(setup.editor.take_failed());
    assert_eq!(
        setup.editor.anchor,
        Some(crate::motion::Point { line: 0, col: 2 })
    );
    // i" again takes the quotes, a" again the next string.
    assert_eq!(taken("x \"ab cd\" y\n", (1, 4), "vi\"i\"y"), "\"ab cd\"");
    assert_eq!(
        taken("x \"ab\" \"cd\" y\n", (1, 3), "va\"a\"y"),
        "\"ab\" \"cd\" "
    );
    // Between two strings, from the quote before, as Vim's.
    assert_eq!(taken("a \"b c\" d \"e\"\n", (1, 8), "yi\""), " d ");
}

#[test]
fn brackets_with_nothing_inside_fail_to_be_selected() {
    // As Vim's current_block in visual mode: nothing in `{` / `}`, nor
    // in the next pair after the cursor.
    for (text, keys) in [("{\n}\n", "jVi{"), ("\t(p q)\t\n\"q\" r\n{\n}\n", "lvi{")] {
        let mut setup = Setup::new(text);
        setup.keys(keys);
        assert!(setup.editor.take_failed(), "{keys}");
        assert_eq!(setup.editor.anchor, Some(setup.editor.cursor.at), "{keys}");
    }
}

#[test]
fn paragraphs_taken_on_keep_the_kind_of_selection() {
    let mode = |keys: &str| {
        let mut setup = Setup::new("a\nb\n\nc\nd\n");
        setup.keys(keys);
        setup.editor.mode
    };
    assert_eq!(mode("<C-v>jap"), Mode::Visual(Selection::Block));
    assert_eq!(mode("vjap"), Mode::Visual(Selection::Characters));
    assert_eq!(mode("Vjap"), Mode::Visual(Selection::Lines));
    // From a line, by lines, as Vim's.
    assert_eq!(mode("<C-v>ap"), Mode::Visual(Selection::Lines));
}

/// Sentences, as check.lua's `prose`.
const PROSE: &str = "One. Two!  Three (four.)  Five\nsix? Seven \"eight.\" Nine.\n\nTen. Last";

/// Where the cursor goes after `keys` from `at` in `PROSE`, the text left
/// unchanged.
fn moved(at: (u64, usize), keys: &str) -> (u64, usize) {
    let (text, cursor, _, _) = after(PROSE, at, keys);
    assert_eq!(text, PROSE, "{keys} changes nothing");
    cursor
}

#[test]
fn parentheses_go_between_sentences_as_vims_do() {
    assert_eq!(moved((1, 0), ")"), (1, 5));
    assert_eq!(moved((1, 0), "3)"), (1, 26));
    assert_eq!(moved((2, 21), "("), (2, 20));
    // Back over an empty line, a paragraph's end, which ends a sentence.
    assert_eq!(moved((4, 0), "2("), (2, 20));
    // At the text's end, onto its last character, which can't be left.
    assert_eq!(moved((4, 6), ")"), (4, 8));
    // Not so many: nowhere.
    assert_eq!(moved((4, 6), "99)"), (4, 6));
}

#[test]
fn operators_take_sentences_as_far_as_the_next() {
    assert_eq!(
        after(PROSE, (1, 5), "d)"),
        (
            PROSE.replacen("Two!  ", "", 1),
            (1, 5),
            "Two!  ".into(),
            false
        )
    );
    // To the text's end, its last character too, as Vim's cursor can't
    // stay past it.
    assert_eq!(
        after(PROSE, (4, 6), "d)"),
        (PROSE.replacen("Last", "L", 1), (4, 5), "ast".into(), false)
    );
}

#[test]
fn is_and_as_take_a_sentence_and_as_its_blanks() {
    assert_eq!(
        after(PROSE, (1, 13), "dis"),
        (
            PROSE.replacen("Three (four.)", "", 1),
            (1, 11),
            "Three (four.)".into(),
            false
        )
    );
    assert_eq!(
        after(PROSE, (1, 13), "das"),
        (
            PROSE.replacen("Three (four.)  ", "", 1),
            (1, 11),
            "Three (four.)  ".into(),
            false
        )
    );
    // Over a line's end, which isn't one's end.
    assert_eq!(after(PROSE, (1, 27), "yas").2, "Five\nsix? ");
    // A count takes the blanks after as one.
    assert_eq!(after(PROSE, (1, 0), "2yis").2, "One. ");
    assert_eq!(
        after(PROSE, (2, 8), "cisX<Esc>").0,
        PROSE.replacen("Seven \"eight.\"", "X", 1)
    );
}

#[test]
fn as_again_in_visual_mode_takes_the_selection_on() {
    assert_eq!(after(PROSE, (1, 6), "vasasy").2, "Two!  Three (four.)  ");
    let mut setup = Setup::new(PROSE);
    setup.keys("vis");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Characters));
}

/// Tags, as check.lua's `html`.
const HTML: &str = "<div><p>Hi <b>bold</b> there</p>\n  <a href=\"x>y\">link</a>\n</div>";

#[test]
fn it_and_at_take_what_is_between_tags_and_the_tags() {
    assert_eq!(
        after(HTML, (1, 14), "dit"),
        (HTML.replacen("bold", "", 1), (1, 14), "bold".into(), false)
    );
    assert_eq!(
        after(HTML, (1, 14), "dat"),
        (
            HTML.replacen("<b>bold</b>", "", 1),
            (1, 11),
            "<b>bold</b>".into(),
            false
        )
    );
    // A count goes out, as far as there are.
    assert_eq!(after(HTML, (1, 14), "2yit").2, "Hi <b>bold</b> there");
    assert_eq!(after(HTML, (2, 4), "3dit").0, HTML);
    // A `>` in quotes in the opening tag isn't its end.
    assert_eq!(after(HTML, (2, 18), "yit").2, "link");
    // In visual mode, again, the tags too.
    assert_eq!(after(HTML, (1, 14), "vitity").2, "<b>bold</b>");
}

#[test]
fn it_takes_the_line_break_before_a_closing_tag_on_a_line_of_its_own() {
    let text = "<p>\ntext\n</p>";
    assert_eq!(
        after(text, (2, 2), "dit"),
        ("<p></p>".into(), (1, 3), "\ntext\n".into(), false)
    );
}

#[test]
fn y_in_visual_mode_copies_the_lines_selected() {
    let four = "alpha beta\ngamma delta\nepsilon\nzeta eta";
    assert_eq!(
        after(four, (1, 3), "vY"),
        (four.into(), (1, 0), "alpha beta\n".into(), true)
    );
    assert_eq!(
        after(four, (1, 6), "vjlY"),
        (
            four.into(),
            (1, 0),
            "alpha beta\ngamma delta\n".into(),
            true
        )
    );
    // A block, as it is.
    let mut setup = Setup::new(four);
    setup.keys("ll<C-v>jlY");
    let register = setup.register();
    assert_eq!(
        (register.text.as_str(), register.block),
        ("ph\nmm", Some(2))
    );
    assert_eq!(setup.cursor(), (0, 2));
}
