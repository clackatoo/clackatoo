//! Block visual mode, Ctrl+V. Every case's text, cursor and register were
//! checked against Neovim 0.11 headless, with `tabstop=4` and
//! `expandtab` as the editor's own options have them, save where
//! clackatoo differs on purpose, which says why.

use super::{Mode, Register, Setup, Wrote};
use crate::pane::Selection;
use crate::render::HighlightKind;

const SIX: &str = "abcdef\nabcdef\nabcdef\n";
const FOUR: &str = "abcdef\nabcdef\nabcdef\nabcdef\n";
const SHORT: &str = "abcdef\na\nabcdef\n";
/// A tab across columns 0 to 3 between two plain lines.
const TAB: &str = "abcdef\n\tab\nabcdef\n";
/// A wide character across columns 1 and 2 between two plain lines.
const WIDE: &str = "abcd\na中b\nabcd\n";

/// The text after typing `keys` into `text`, back in normal mode.
fn after(text: &str, keys: &str) -> String {
    let mut setup = Setup::new(text);
    setup.keys(keys);
    assert_eq!(setup.editor.mode, Mode::Normal, "{keys:?} on {text:?}");
    setup.text()
}

/// The text and the cursor after typing `keys` into `text`.
fn with_cursor(text: &str, keys: &str) -> (String, (u64, usize)) {
    let mut setup = Setup::new(text);
    setup.keys(keys);
    (setup.text(), setup.cursor())
}

/// What `p` puts after typing `keys` into `text`, and its block's width.
fn kept(text: &str, keys: &str) -> (String, Option<usize>) {
    let mut setup = Setup::new(text);
    setup.keys(keys);
    let register = setup.register();
    (register.text, register.block)
}

/// `text` with `rows`, a block `width` wide, in the register `a`.
fn with_block(text: &str, rows: &[&str], width: usize) -> Setup {
    let mut setup = Setup::new(text);
    let rows: Vec<String> = rows.iter().map(|&row| row.to_owned()).collect();
    let register = Register::block(&rows, width);
    setup
        .shared
        .registers
        .wrote(Some('a'), register, Wrote::Yank);
    setup
}

#[test]
fn ctrl_v_selects_a_block_and_switches_with_v_and_shift_v() {
    let mut setup = Setup::new(SIX);
    setup.keys("<C-v>");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Block));
    setup.keys("v");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Characters));
    setup.keys("<C-v>V");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Lines));
    setup.keys("<C-v><C-v>");
    assert_eq!(setup.editor.mode, Mode::Normal);
    // From each other kind, the same corners.
    assert_eq!(after(SIX, "lvjj<C-v>d"), "acdef\nacdef\nacdef\n");
    assert_eq!(after(SIX, "l<C-v>jjvd"), "acdef\n");
    assert_eq!(after(SIX, "l<C-v>jjVd"), "");
    // Twice is out again, and `x` takes one character.
    assert_eq!(
        with_cursor(SIX, "l<C-v>jj<C-v>x"),
        ("abcdef\nabcdef\nacdef\n".into(), (2, 1))
    );
}

#[test]
fn d_takes_the_same_columns_of_each_line_from_either_corner() {
    assert_eq!(
        with_cursor(SIX, "l<C-v>jjld"),
        ("adef\nadef\nadef\n".into(), (0, 1))
    );
    assert_eq!(kept(SIX, "l<C-v>jjld"), ("bc\nbc\nbc".into(), Some(2)));
    assert_eq!(after(SIX, "l<C-v>jjlx"), "adef\nadef\nadef\n");
    assert_eq!(
        with_cursor(SIX, "2j3l<C-v>khhd"),
        ("abcdef\naef\naef\n".into(), (1, 1))
    );
    assert_eq!(after(SIX, "2jl<C-v>kkld"), "adef\nadef\nadef\n");
    // A line that ends before the block keeps all it has, and is kept as
    // spaces; one that ends inside it, as much as it has.
    assert_eq!(after(SHORT, "ll<C-v>jjld"), "abef\na\nabef\n");
    assert_eq!(kept(SHORT, "ll<C-v>jjly"), ("cd\n  \ncd".into(), Some(2)));
    let partial = "abcdef\nabc\nabcdef\n";
    assert_eq!(after(partial, "ll<C-v>jjld"), "abef\nab\nabef\n");
    assert_eq!(kept(partial, "ll<C-v>jjly").0, "cd\nc\ncd");
}

#[test]
fn y_keeps_a_block_and_leaves_the_cursor_at_its_top_left() {
    assert_eq!(with_cursor(SIX, "l<C-v>jjly"), (SIX.into(), (0, 1)));
    assert_eq!(kept(SIX, "l<C-v>jjly"), ("bc\nbc\nbc".into(), Some(2)));
    let two = "abcdef\nabcdef\n";
    assert_eq!(with_cursor(two, "j3l<C-v>khy"), (two.into(), (0, 2)));
    assert_eq!(kept(two, "j3l<C-v>khy").0, "cd\ncd");
    // `:registers` says it's a block.
    let mut setup = Setup::new(two);
    setup.keys("<C-v>jly");
    assert!(setup.shared.registers.listing()[0].starts_with("b  \"\"   ab^Jab"));
}

#[test]
fn an_edge_splits_a_tab_or_a_wide_character_into_spaces() {
    // A tab from column 2 to 3, the left edge at 3.
    let tab_left = "ab\tcd\nabcdefgh\n";
    assert_eq!(
        with_cursor(tab_left, "j3l<C-v>kld"),
        ("ab d\nabcfgh\n".into(), (0, 3))
    );
    assert_eq!(kept(tab_left, "j3l<C-v>kly").0, " c\nde");
    // On the tab, the cursor is at its start, the block's corner within.
    assert_eq!(with_cursor(tab_left, "j3l<C-v>kly").1, (0, 2));
    // A tab from column 1 to 3, the right edge at 1.
    let tab_right = "a\tbc\nabcdefgh\n";
    assert_eq!(after(tab_right, "<C-v>jld"), "  bc\ncdefgh\n");
    assert_eq!(kept(tab_right, "<C-v>jly").0, "a \nab");
    // One tab cut by both edges.
    assert_eq!(after(TAB, "l<C-v>jjld"), "adef\n  ab\nadef\n");
    assert_eq!(kept(TAB, "l<C-v>jjly").0, "bc\n  \nbc");
    // A wide character cut by the left edge, then by the right.
    let wide_left = "a中b\nabcde\n";
    assert_eq!(
        with_cursor(wide_left, "jll<C-v>lkd"),
        ("a \nabe\n".into(), (0, 1))
    );
    assert_eq!(kept(wide_left, "jll<C-v>lky").0, " b\ncd");
    assert_eq!(after(WIDE, "<C-v>jjld"), "cd\n b\ncd\n");
    assert_eq!(kept(WIDE, "<C-v>jjly").0, "ab\na \nab");
    // Wholly inside, it's taken whole.
    assert_eq!(kept(WIDE, "l<C-v>jjly").0, "bc\n中\nbc");
}

#[test]
fn dollar_takes_every_line_to_its_end() {
    let ragged = "abc\nabcdef\nab\n";
    assert_eq!(after(ragged, "l<C-v>jj$d"), "a\na\na\n");
    assert_eq!(kept(ragged, "l<C-v>jj$y"), ("bc\nbcdef\nb".into(), Some(5)));
    // It stays to the ends moving down.
    assert_eq!(after("abc\nabcdef\n", "l<C-v>$jd"), "a\na\n");
    // As wide as the widest line.
    assert_eq!(kept("abcdef\na\nabc\n", "<C-v>jj$y").1, Some(6));
    // A line that ends before the block gives nothing, where Neovim gives
    // five spaces whatever the block.
    assert_eq!(kept("abcdef\na\nabcd\n", "ll<C-v>jj$y").0, "cdef\n\ncd");
    assert_eq!(after("abc\nABCDEF\n", "<C-v>j$~"), "ABC\nabcdef\n");
}

#[test]
fn i_types_before_the_block_on_every_line_that_reaches_it() {
    assert_eq!(
        with_cursor(SIX, "ll<C-v>jjIXY<Esc>"),
        ("abXYcdef\nabXYcdef\nabXYcdef\n".into(), (0, 2))
    );
    // A line that ends before the block is left as it is, even after `$`;
    // one that ends at its edge isn't.
    assert_eq!(after(SHORT, "ll<C-v>jjIXY<Esc>"), "abXYcdef\na\nabXYcdef\n");
    assert_eq!(
        after(SHORT, "ll<C-v>jj$IXY<Esc>"),
        "abXYcdef\na\nabXYcdef\n"
    );
    assert_eq!(
        after("abcdef\nab\nabcdef\n", "ll<C-v>jjIX<Esc>"),
        "abXcdef\nabX\nabXcdef\n"
    );
    assert_eq!(after("abc\n\nabc\n", "<C-v>jjIX<Esc>"), "Xabc\nX\nXabc\n");
    // A tab the edge is inside is split; a wide character keeps its place
    // after what's typed, spaces before it reaching the column.
    assert_eq!(
        after(TAB, "ll<C-v>jjIX<Esc>"),
        "abXcdef\n  X  ab\nabXcdef\n"
    );
    assert_eq!(
        after("\tabc\n\tabc\n", "l<C-v>jIX<Esc>"),
        "\tXabc\n\tXabc\n"
    );
    assert_eq!(after(WIDE, "ll<C-v>jjIX<Esc>"), "abXcd\na X中b\nabXcd\n");
    // From the bottom right, the cursor goes to the top left.
    let two = "abcdef\nabcdef\n";
    assert_eq!(
        with_cursor(two, "j3l<C-v>khIXY<Esc>"),
        ("abXYcdef\nabXYcdef\n".into(), (0, 2))
    );
}

#[test]
fn what_i_spreads_is_what_the_first_line_gained() {
    // Corrected as typed, or typed after moving back within it.
    assert_eq!(
        after(SIX, "ll<C-v>jjIXY<BS>Z<Esc>"),
        "abXZcdef\nabXZcdef\nabXZcdef\n"
    );
    assert_eq!(
        after("abcdef\nabcdef\n", "ll<C-v>jIX<Left>Y<Esc>"),
        "abYXcdef\nabYXcdef\n"
    );
    // With a count, as many times.
    assert_eq!(
        after(SIX, "ll<C-v>jj2IX<Esc>"),
        "abXXcdef\nabXXcdef\nabXXcdef\n"
    );
    // A line break typed goes on the first line only.
    assert_eq!(
        with_cursor(SIX, "l<C-v>jjlcX<CR>Y<Esc>"),
        ("aX\nYdef\nadef\nadef\n".into(), (1, 0))
    );
    // Nothing typed changes nothing, and the cursor steps back.
    assert_eq!(
        with_cursor("abcdef\nabcdef\n", "ll<C-v>jI<Esc>"),
        ("abcdef\nabcdef\n".into(), (0, 1))
    );
}

#[test]
fn a_types_after_the_block_padding_short_lines_or_at_each_end_after_dollar() {
    assert_eq!(
        with_cursor(SIX, "l<C-v>jjlAXY<Esc>"),
        ("abcXYdef\nabcXYdef\nabcXYdef\n".into(), (0, 1))
    );
    assert_eq!(
        after(SHORT, "ll<C-v>jjlAXY<Esc>"),
        "abcdXYef\na   XY\nabcdXYef\n"
    );
    assert_eq!(after("abc\n\nabc\n", "<C-v>jjAX<Esc>"), "aXbc\n X\naXbc\n");
    assert_eq!(
        after("abcdef\nabc\nabcdef\n", "ll<C-v>jjlAX<Esc>"),
        "abcdXef\nabc X\nabcdXef\n"
    );
    assert_eq!(
        after("abcdef\nabcd\nabcdef\n", "ll<C-v>jjlAX<Esc>"),
        "abcdXef\nabcdX\nabcdXef\n"
    );
    assert_eq!(
        after("abc\nabcdef\na\n", "<C-v>jj$AX<Esc>"),
        "abcX\nabcdefX\naX\n"
    );
    assert_eq!(after("abc\n\nab\n", "l<C-v>jj$AX<Esc>"), "abcX\nX\nabX\n");
    assert_eq!(
        after("abcdef\na\nabc\n", "3l<C-v>jj$AX<Esc>"),
        "abcdefX\naX\nabcX\n"
    );
    assert_eq!(after(TAB, "l<C-v>jjAX<Esc>"), "abXcdef\n  X  ab\nabXcdef\n");
    assert_eq!(after(WIDE, "<C-v>jjlAX<Esc>"), "abXcd\na X中b\nabXcd\n");
    // A wide character at a corner is in the block whole.
    assert_eq!(after("abcd\n中文\n", "<C-v>jAX<Esc>"), "abXcd\n中X文\n");
}

#[test]
fn c_takes_the_block_and_types_in_its_place_on_each_line() {
    assert_eq!(
        with_cursor(SIX, "l<C-v>jjlcXY<Esc>"),
        ("aXYdef\naXYdef\naXYdef\n".into(), (0, 2))
    );
    assert_eq!(kept(SIX, "l<C-v>jjlcXY<Esc>").0, "bc\nbc\nbc");
    assert_eq!(after(SHORT, "ll<C-v>jjlcX<Esc>"), "abXef\na\nabXef\n");
    assert_eq!(
        after("abcdef\nabc\nabcdef\n", "ll<C-v>jjlcX<Esc>"),
        "abXef\nabX\nabXef\n"
    );
    let at_edge = "abcdef\nab\nabcdef\n";
    assert_eq!(after(at_edge, "ll<C-v>jjlcX<Esc>"), "abXef\nabX\nabXef\n");
    assert_eq!(kept(at_edge, "ll<C-v>jjlcX<Esc>").0, "cd\n\ncd");
    assert_eq!(after("abc\nabcdef\n", "l<C-v>j$cX<Esc>"), "aX\naX\n");
    assert_eq!(after(TAB, "l<C-v>jjlcX<Esc>"), "aXdef\n X ab\naXdef\n");
    assert_eq!(
        with_cursor(SIX, "l<C-v>jjlc<Esc>"),
        ("adef\nadef\nadef\n".into(), (0, 0))
    );
    // `D` and `C` go from the left edge to each line's end.
    assert_eq!(after("abcdef\nabcdef\n", "ll<C-v>jD"), "ab\nab\n");
    assert_eq!(
        kept("abcdef\nabcdef\n", "ll<C-v>jD"),
        ("cdef\ncdef".into(), Some(4))
    );
    assert_eq!(after("abcdef\nabcdef\n", "ll<C-v>jCX<Esc>"), "abX\nabX\n");
}

#[test]
fn r_and_case_changes_act_on_whats_in_the_block() {
    assert_eq!(
        with_cursor("abcdef\nabcdef\n", "l<C-v>jlrx"),
        ("axxdef\naxxdef\n".into(), (0, 1))
    );
    assert_eq!(after(TAB, "l<C-v>jjlrx"), "axxdef\n xx ab\naxxdef\n");
    assert_eq!(after(SHORT, "l<C-v>jjlrx"), "axxdef\na\naxxdef\n");
    assert_eq!(after(WIDE, "<C-v>jjlrx"), "xxcd\nxx b\nxxcd\n");
    assert_eq!(after(WIDE, "ll<C-v>jjlrx"), "abxx\na xx\nabxx\n");
    assert_eq!(after("abc\nabcdef\n", "l<C-v>j$rx"), "axx\naxxxxx\n");
    assert_eq!(after("abcdef\nABCDEF\n", "l<C-v>jl~"), "aBCdef\nAbcDEF\n");
    assert_eq!(after("abcdef\nabcdef\n", "l<C-v>jlU"), "aBCdef\naBCdef\n");
    assert_eq!(
        after("ABCDEF\nA\nABCDEF\n", "l<C-v>jjlu"),
        "AbcDEF\nA\nAbcDEF\n"
    );
    // A wide character an edge cuts is left as it is.
    assert_eq!(after(WIDE, "ll<C-v>jjl~"), "abCD\na中B\nabCD\n");
}

#[test]
fn shifting_moves_the_text_from_the_left_edge_by_shiftwidth() {
    let two = "abcdef\nabcdef\n";
    assert_eq!(
        with_cursor(two, "ll<C-v>j>"),
        ("ab    cdef\nab    cdef\n".into(), (0, 2))
    );
    assert_eq!(after(two, "ll<C-v>j2>"), "ab        cdef\nab        cdef\n");
    // Blanks at the edge, or just before it, grow.
    assert_eq!(
        after("ab  cdef\nabcdef\n", "ll<C-v>j>"),
        "ab      cdef\nab    cdef\n"
    );
    assert_eq!(
        after("a  bcdef\na  bcdef\n", "3l<C-v>j>"),
        "a      bcdef\na      bcdef\n"
    );
    assert_eq!(
        after("abcdef\na\nabcdef\n", "ll<C-v>jj>"),
        "ab    cdef\na\nab    cdef\n"
    );
    // Back, by as many blanks as there are from the edge.
    assert_eq!(
        after("ab    cdef\nab    cdef\n", "ll<C-v>j<lt>"),
        "abcdef\nabcdef\n"
    );
    assert_eq!(
        after("ab  cdef\nab      cdef\n", "ll<C-v>j<lt>"),
        "abcdef\nab  cdef\n"
    );
    assert_eq!(
        after("abcdef\nab  cdef\n", "ll<C-v>j<lt>"),
        "abcdef\nabcdef\n"
    );
    assert_eq!(
        after("a      bc\na      bc\n", "3l<C-v>j<lt>"),
        "a  bc\na  bc\n"
    );
    // With tabs, to each stop they reach.
    let mut setup = Setup::new(two);
    setup.set(|options| options.expandtab = false);
    setup.keys("ll<C-v>j>");
    assert_eq!(setup.text(), "ab\t  cdef\nab\t  cdef\n");
}

#[test]
fn p_puts_a_block_a_row_on_each_line_padding_where_text_follows() {
    let three = "abc\nabc\nabc\n";
    assert_eq!(
        with_cursor(three, "<C-v>jly$p"),
        ("abcab\nabcab\nabc\n".into(), (0, 3))
    );
    assert_eq!(
        with_cursor(three, "<C-v>jly$P"),
        ("ababc\nababc\nabc\n".into(), (0, 2))
    );
    assert_eq!(
        with_cursor("abcd\nabcd\nabcd\n", "<C-v>jly0p"),
        ("aabbcd\naabbcd\nabcd\n".into(), (0, 1))
    );
    let cases: [(&str, &[&str], usize, &str, &str); 10] = [
        (
            "abc\na\nabcdef\n",
            &["XY", "ZW"],
            2,
            "ll\"ap",
            "abcXY\na  ZW\nabcdef\n",
        ),
        (
            "abc\na\nabcdef\n",
            &["X", "ZWV"],
            3,
            "ll\"ap",
            "abcX\na  ZWV\nabcdef\n",
        ),
        ("abc\nabc\n", &["X", "ZWV"], 3, "ll\"ap", "abcX\nabcZWV\n"),
        ("abc\nabc\n", &["X", "ZWV"], 3, "\"ap", "aX  bc\naZWVbc\n"),
        ("abc\n", &["XY", "ZW"], 2, "l\"ap", "abXYc\n  ZW\n"),
        (
            "abcde\na\tb\n",
            &["XY", "ZW"],
            2,
            "l\"ap",
            "abXYcde\na ZW  b\n",
        ),
        (
            "abcde\na中b\n",
            &["XY", "ZW"],
            2,
            "l\"ap",
            "abXYcde\na ZW中b\n",
        ),
        (
            "abc\nabc\n",
            &["XY", "ZW"],
            2,
            "2\"ap",
            "aXYXYbc\naZWZWbc\n",
        ),
        ("abc\n\n", &["XY", "ZW"], 2, "ll\"ap", "abcXY\n   ZW\n"),
        (
            "abc\na\nabcdef\n",
            &["XY", "ZW"],
            2,
            "ll\"aP",
            "abXYc\na ZW\nabcdef\n",
        ),
    ];
    for (text, rows, width, keys, put) in cases {
        let mut setup = with_block(text, rows, width);
        setup.keys(keys);
        assert_eq!(setup.text(), put, "{keys:?} on {text:?}");
    }
    let mut setup = with_block("abc\nabc\n", &["XY", "ZW"], 2);
    setup.keys("\"aP");
    assert_eq!(
        (setup.text(), setup.cursor()),
        ("XYabc\nZWabc\n".into(), (0, 0))
    );
    // A block kept to the ends is as wide as its widest row.
    assert_eq!(
        after("abcdef\na\nabc\n", "<C-v>jj$y0P"),
        "abcdefabcdef\na     a\nabc   abc\n"
    );
}

#[test]
fn p_over_a_block_puts_a_block_in_its_place_or_text_on_each_line() {
    let two = "abcdef\nabcdef\n";
    let mut setup = with_block(two, &["XY", "ZW"], 2);
    setup.keys("l<C-v>jl\"ap");
    assert_eq!(setup.text(), "aXYdef\naZWdef\n");
    // What was there is kept, as a block.
    assert_eq!(
        setup.register(),
        Register::block(&["bc".into(), "bc".into()], 2)
    );
    let mut setup = Setup::new(two);
    setup
        .shared
        .registers
        .wrote(Some('a'), Register::chars("Q".into()), Wrote::Yank);
    setup.keys("l<C-v>jl\"ap");
    assert_eq!(setup.text(), "aQdef\naQdef\n");
    let mut setup = Setup::new(two);
    setup
        .shared
        .registers
        .wrote(Some('a'), Register::new("Q\n".into(), true), Wrote::Yank);
    setup.keys("l<C-v>jl\"ap");
    assert_eq!(
        (setup.text(), setup.cursor()),
        ("adef\nadef\nQ\n".into(), (2, 0))
    );
    // A block over characters goes where they were.
    let mut setup = with_block(two, &["XY", "ZW"], 2);
    setup.keys("lvl\"ap");
    assert_eq!(setup.text(), "aXYdef\naZWbcdef\n");
}

#[test]
fn dot_makes_a_block_change_again_on_as_big_a_block_from_the_cursor() {
    let cases = [
        (
            FOUR,
            "<C-v>jIX<Esc>2jl.",
            "Xabcdef\nXabcdef\naXbcdef\naXbcdef\n",
        ),
        (FOUR, "<C-v>jld2jll.", "cdef\ncdef\nabef\nabef\n"),
        (
            "ab\nabcd\na\nabc\n",
            "<C-v>j$AX<Esc>2j0.",
            "abX\nabcdX\naX\nabcX\n",
        ),
        (FOUR, "<C-v>jlcZ<Esc>2jll.", "Zcdef\nZcdef\nabZef\nabZef\n"),
        (FOUR, "<C-v>jlrQ2jll.", "QQcdef\nQQcdef\nabQQef\nabQQef\n"),
        (
            FOUR,
            "<C-v>jlAZ<Esc>2jll.",
            "abZcdef\nabZcdef\nabcdZef\nabcdZef\n",
        ),
        (FOUR, "<C-v>jl~2jll.", "ABcdef\nABcdef\nabCDef\nabCDef\n"),
        (
            FOUR,
            "l<C-v>j>2jl.",
            "a    bcdef\na    bcdef\nab    cdef\nab    cdef\n",
        ),
        // From a tab, its first column, and no further than the block's
        // width, though the tab's wider.
        (
            "abcdef\nabcdef\n\tabcdef\nabcdef\n",
            "<C-v>jld2j0.",
            "cdef\ncdef\n  abcdef\ncdef\n",
        ),
        // A count changes nothing.
        (
            "abc\nabc\nabc\nabc\n",
            "<C-v>jIX<Esc>2jl3.",
            "Xabc\nXabc\naXbc\naXbc\n",
        ),
        (FOUR, "<C-v>jld2j03.", "cdef\ncdef\ncdef\ncdef\n"),
        (FOUR, "<C-v>jlcZ<Esc>2j02.", "Zcdef\nZcdef\nZcdef\nZcdef\n"),
        // Past the last line, no further.
        (
            "abcdef\nabcdef\nabcdef\n",
            "<C-v>jld2j0.",
            "cdef\ncdef\ncdef\n",
        ),
    ];
    for (text, keys, changed) in cases {
        assert_eq!(after(text, keys), changed, "{keys:?} on {text:?}");
    }
}

#[test]
fn every_block_change_is_one_undo_step() {
    for keys in [
        "l<C-v>jjld",
        "ll<C-v>jjIXY<Esc>",
        "l<C-v>jjlAXY<Esc>",
        "l<C-v>jjlcXY<Esc>",
        "l<C-v>jjlrx",
        "l<C-v>jjl~",
        "ll<C-v>jj>",
        "l<C-v>jly$p",
    ] {
        let mut setup = Setup::new(SIX);
        setup.keys(keys);
        assert_ne!(setup.text(), SIX, "{keys:?}");
        setup.keys("u");
        assert_eq!(setup.text(), SIX, "{keys:?} undone in one step");
    }
}

#[test]
fn o_and_shift_o_go_to_the_other_corners() {
    let two = "abcdef\nabcdef\n";
    assert_eq!(with_cursor(two, "l<C-v>jlo<Esc>").1, (0, 1));
    assert_eq!(with_cursor(two, "l<C-v>jlO<Esc>").1, (1, 1));
    assert_eq!(after(two, "l<C-v>jlOhd"), "def\ndef\n");
    assert_eq!(after(two, "l<C-v>jlOOld"), "aef\naef\n");
    // Across a tab, to its start.
    assert_eq!(
        with_cursor("ab\tcd\nabcdefgh\n", "<C-v>jlllO<Esc>").1,
        (1, 0)
    );
    // In a plain selection, as `o`.
    assert_eq!(with_cursor(two, "lvjlO<Esc>").1, (0, 1));
}

#[test]
fn j_and_gv_and_the_command_line_take_the_blocks_lines() {
    assert_eq!(after("a\nb\nc\n", "<C-v>jJ"), "a b\nc\n");
    assert_eq!(
        after("abcdef\nabcdef\n", "l<C-v>jl<Esc>gvd"),
        "adef\nadef\n"
    );
    let mut setup = Setup::new(SIX);
    setup.keys("ll<C-v>j");
    let line = setup
        .editor
        .command_line(&setup.copy, &mut setup.shared.marks);
    assert_eq!(line, "'<,'>");
    assert_eq!(setup.editor.mode, Mode::Normal);
    setup.keys("gvd");
    assert_eq!(setup.text(), "abdef\nabdef\nabcdef\n");
}

#[test]
fn a_block_is_drawn_over_each_lines_text_from_its_left_edge() {
    let mut setup = Setup::new("abcdef\na\n\tab\nabcdef\n");
    setup.keys("l<C-v>jjjl");
    let gutter = setup.editor.gutter;
    let at = |cols: std::ops::Range<usize>| gutter + cols.start..gutter + cols.end;
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, at(1..3)), (2, at(1..3)), (3, at(1..3))]
    );
    setup.keys("$");
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, at(1..6)), (2, at(1..6)), (3, at(1..6))]
    );
}

#[test]
fn a_wrapped_line_has_the_block_on_the_row_its_columns_are_on() {
    let mut setup = Setup::new("abcdefghijkl\nabcdefghijkl\n");
    setup.set(|options| {
        options.number = false;
        options.wrap = true;
    });
    setup
        .editor
        .resize(clackatoo_protocol::Size::new(8, 10).unwrap());
    // Columns 6 to 9: the end of each line's first row, the start of its
    // second.
    setup.keys("6l<C-v>j3l");
    assert_eq!(
        setup.highlighted(HighlightKind::Selection),
        [(0, 6..8), (1, 0..2), (2, 6..8), (3, 0..2)]
    );
}

#[test]
fn a_block_in_a_listing_changes_names_and_never_a_mark_or_dot_dot() {
    use super::visual::listing;
    let (mut setup, [one, two, three]) = listing();
    setup.keys("<C-v>jjjlld");
    assert_eq!(setup.text(), format!("..\n{one}ha\n{two}a/\n{three}ma\n"));
    let (mut setup, [one, two, three]) = listing();
    setup.keys("j<C-v>jjIx-<Esc>");
    assert_eq!(
        setup.text(),
        format!("..\n{one}x-alpha\n{two}x-beta/\n{three}x-gamma\n")
    );
    let (mut setup, [one, two, three]) = listing();
    setup.keys("<C-v>jjjlrx");
    assert_eq!(
        setup.text(),
        format!("..\n{one}xxpha\n{two}xxta/\n{three}xxmma\n")
    );
    // Typing that would start on `..` is refused.
    let (mut setup, _) = listing();
    let before = setup.text();
    setup.keys("<C-v>jI");
    assert_eq!(setup.editor.mode, Mode::Normal);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some(super::super::block::ON_PARENT)
    );
    // As is shifting, and a block put past the listing's end.
    let (mut setup, _) = listing();
    setup.keys("j<C-v>j>");
    assert_eq!(setup.text(), before);
    assert_eq!(setup.editor.mode, Mode::Normal);
    let (mut setup, _) = listing();
    setup.keys("jj<C-v>jly");
    setup.keys("Gp");
    assert_eq!(setup.text(), before);
    assert!(
        setup
            .editor
            .status
            .as_deref()
            .is_some_and(|said| said.contains("past"))
    );
    // Nor on `..`, which would then name something, put or put over.
    let on_parent = [
        ("ggp", super::super::block::PUT_ON_PARENT),
        ("ggP", super::super::block::PUT_ON_PARENT),
        ("gg<C-v>jl\"0p", super::super::block::ON_PARENT),
        ("ggj<C-v>kl\"0p", super::super::block::ON_PARENT),
    ];
    for (keys, said) in on_parent {
        let (mut setup, _) = listing();
        setup.keys("jj<C-v>jly");
        setup.keys(keys);
        assert_eq!(setup.text(), before, "{keys}");
        assert_eq!(setup.editor.status.as_deref(), Some(said), "{keys}");
    }
}

/// Has an agent put `inserted` at byte `at` of the text, as the desk hears
/// it.
fn agent_puts(setup: &mut Setup, at: usize, inserted: &str) {
    let update = setup.daemon.agent(at, inserted);
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(update);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
}

#[test]
fn an_agents_edit_to_a_blocks_lines_while_typing_types_on_the_first_only() {
    // On a line below the first: what's typed stays on the first.
    let mut setup = Setup::new(SIX);
    setup.keys("ll<C-v>jjIXY");
    agent_puts(&mut setup, 10, "!");
    setup.keys("<Esc>");
    assert_eq!(setup.text(), "abXYcdef\na!bcdef\nabcdef\n");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some(super::super::block::SPOILED)
    );
    // On the first line, after where typing goes.
    let mut setup = Setup::new(SIX);
    setup.keys("ll<C-v>jjIZ");
    agent_puts(&mut setup, 6, "?");
    setup.keys("<Esc>");
    assert_eq!(setup.text(), "abZcde?f\nabcdef\nabcdef\n");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some(super::super::block::SPOILED)
    );
}

#[test]
fn nothing_is_put_over_a_selection_starting_on_a_listings_dot_dot() {
    use super::visual::listing;
    let (setup, _) = listing();
    let before = setup.text();
    // Characters over several lines, over a block, or over characters.
    for keys in ["gg<C-v>jl\"ap", "ggvl\"ap", "ggvjl\"aP"] {
        let (mut setup, _) = listing();
        let register = Register::chars("x\ny".into());
        setup
            .shared
            .registers
            .wrote(Some('a'), register, Wrote::Yank);
        setup.keys(keys);
        assert_eq!(setup.text(), before, "{keys}");
        assert_eq!(
            setup.editor.status.as_deref(),
            Some(super::super::block::ON_PARENT),
            "{keys}"
        );
    }
}

#[test]
fn an_agents_edit_beside_a_block_while_typing_leaves_it_typed_on_every_line() {
    // Lines put in above it, and below it.
    let mut setup = Setup::new("top\nabcdef\nabcdef\nend\n");
    setup.keys("jll<C-v>jIX");
    agent_puts(&mut setup, 0, "// a\n// b\n");
    let end = setup.copy.text().len_bytes();
    agent_puts(&mut setup, end, "more\n");
    setup.keys("<Esc>");
    assert_eq!(
        setup.text(),
        "// a\n// b\ntop\nabXcdef\nabXcdef\nend\nmore\n"
    );
    assert_eq!(setup.editor.status, None);
}

#[test]
fn shift_left_keeps_a_tab_that_still_fits_and_o_makes_a_dollar_block_fixed() {
    assert_eq!(
        after("ab\t\tcd\nab\t\tcd\n", "ll<C-v>j<lt>"),
        "ab\tcd\nab\tcd\n"
    );
    assert_eq!(
        after("ab  \t  cd\nab  \t  cd\n", "ll<C-v>j<lt>"),
        "ab    cd\nab    cd\n"
    );
    assert_eq!(after("ab\t\t\tcd\n", "ll<C-v><lt>"), "ab\t\tcd\n");
    // `O` after `$`: from the corner to just past the cursor's line's end,
    // the shorter line's corner just past its own.
    assert_eq!(after("abc\nabcdef\n", "l<C-v>j$Od"), "a\naef\n");
    assert_eq!(after("abcdefgh\nabcdefgh\n", "l<C-v>j$Ohd"), "\n\n");
}

#[test]
fn putting_over_a_block_and_typing_across_a_split_tab_are_one_undo_step() {
    let two = "abcdef\nabcdef\n";
    let mut setup = with_block(two, &["XY", "ZW"], 2);
    setup.keys("l<C-v>jl\"ap");
    assert_eq!(setup.text(), "aXYdef\naZWdef\n");
    setup.keys("u");
    assert_eq!(setup.text(), two);
    let mut setup = Setup::new(TAB);
    setup.keys("ll<C-v>jjIX<Esc>");
    assert_eq!(setup.text(), "abXcdef\n  X  ab\nabXcdef\n");
    setup.keys("u");
    assert_eq!(setup.text(), TAB);
}
