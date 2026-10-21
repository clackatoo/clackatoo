//! The keys that go by the view: paging (Ctrl+D, Ctrl+U, Ctrl+F, Page Down
//! and Page Up), Ctrl+E and Ctrl+Y, and `H`, `M` and `L`, each case checked
//! against Neovim 0.11 headless (`crates/gui/tests/nvim/check.lua`), in a
//! view of ten rows. Where clackatoo differs on purpose: Ctrl+E and Ctrl+Y
//! go by screen rows where lines wrap, as the wheel does, not by lines.

use std::fmt::Write;

use clackatoo_protocol::Size;

use super::super::wrap::Top;
use super::Setup;

/// Sixty lines, every other one indented, as check.lua's `sixty`.
fn sixty() -> String {
    (1..=60).fold(String::new(), |mut text, n| {
        let indent = if n % 2 == 0 { "  " } else { "" };
        let _ = writeln!(text, "{indent}line {n}");
        text
    })
}

/// Sixty lines, the odd ones long enough to wrap over two rows of 20, as
/// check.lua's `wrapping`.
fn wrapping() -> String {
    (1..=60).fold(String::new(), |mut text, n| {
        let _ = if n % 2 == 1 {
            writeln!(text, "{n:02}{}", "x".repeat(28))
        } else {
            writeln!(text, "line {n}")
        };
        text
    })
}

/// An editor showing `text` ten rows tall, 20 columns of text wide if
/// `wrap`, with no `scrolloff`, its view's top at line `top` and its cursor
/// at `line` and byte `col`, counting lines from 1 as check.lua does.
fn viewed(text: &str, top: u64, (line, col): (u64, usize), wrap: bool) -> Setup {
    let mut setup = Setup::new(text);
    setup.set(|options| {
        options.scrolloff = 0;
        options.number = false;
        options.relativenumber = false;
        options.wrap = wrap;
    });
    setup
        .editor
        .resize(Size::new(if wrap { 20 } else { 40 }, 10).unwrap());
    setup.keys(&format!("{line}G0"));
    for _ in 0..col {
        setup.keys("l");
    }
    setup.editor.top = Top {
        line: top - 1,
        cell: 0,
    };
    setup
}

/// The view's top line, and the cursor's line and byte, counting lines
/// from 1 as check.lua does, after `keys`.
fn after(mut setup: Setup, keys: &str) -> (u64, u64, usize) {
    setup.keys(keys);
    let (top, line, col) = setup.view();
    (top + 1, line + 1, col)
}

#[test]
fn half_pages_move_the_view_and_the_cursor_and_a_count_sets_how_far() {
    let text = sixty();
    assert_eq!(
        after(viewed(&text, 11, (15, 4), false), "<C-d>"),
        (16, 20, 4)
    );
    assert_eq!(
        after(viewed(&text, 11, (15, 4), false), "3<C-d><C-d>"),
        (17, 21, 4)
    );
    assert_eq!(
        after(viewed(&text, 21, (25, 4), false), "<C-u>"),
        (16, 20, 4)
    );
    // No further than the view.
    assert_eq!(
        after(viewed(&text, 11, (15, 4), false), "20<C-d>"),
        (21, 25, 4)
    );
}

#[test]
fn half_pages_show_nothing_past_the_texts_end_but_the_cursor_goes_on() {
    let text = sixty();
    let at = |top, cursor, keys| after(viewed(&text, top, (cursor, 4), false), keys);
    // Three rows to line 60 at the bottom; the cursor five.
    assert_eq!(at(48, 50, "<C-d>"), (51, 55, 4));
    // None at all.
    assert_eq!(at(51, 55, "<C-d>"), (51, 60, 4));
    let short = viewed("a\nb\nc\n", 1, (1, 0), false);
    assert_eq!(after(short, "<C-d>"), (1, 3, 0));
    // Up, as far as the text's start, and the cursor on.
    assert_eq!(at(3, 8, "<C-u>"), (1, 3, 4));
}

#[test]
fn where_lines_wrap_half_pages_go_to_a_lines_start() {
    let text = wrapping();
    // Five rows down from line 12 end on line 15's second: back to its
    // first, the cursor four rows.
    assert_eq!(
        after(viewed(&text, 12, (13, 2), true), "<C-d>"),
        (15, 15, 22)
    );
    // Five up from line 21 end on line 17's second, four lines back: on
    // to line 18, the cursor four rows.
    assert_eq!(
        after(viewed(&text, 21, (24, 2), true), "<C-u>"),
        (18, 21, 22)
    );
}

#[test]
fn pages_move_the_view_less_two_rows_and_the_cursor_to_its_top_or_bottom() {
    let text = sixty();
    assert_eq!(
        after(viewed(&text, 11, (15, 4), false), "<C-f>"),
        (19, 19, 4)
    );
    assert_eq!(
        after(viewed(&text, 11, (15, 4), false), "2<C-f>"),
        (27, 27, 4)
    );
    assert_eq!(
        after(viewed(&text, 21, (25, 4), false), "<PageUp>"),
        (13, 22, 4)
    );
    // With `scrolloff`, the cursor that many rows into it.
    let mut setup = viewed(&text, 11, (15, 4), false);
    setup.set(|options| options.scrolloff = 3);
    assert_eq!(after(setup, "<C-f>"), (19, 22, 4));
}

#[test]
fn a_page_puts_the_cursor_at_the_views_top_or_bottom_line_wherever_it_was() {
    let text = sixty();
    let at = |top, cursor, keys| after(viewed(&text, top, (cursor, 4), false), keys);
    // Still in view after the page, but it goes to the top all the same.
    assert_eq!(at(11, 20, "<C-f>"), (19, 19, 4));
    // Or the bottom.
    assert_eq!(at(21, 21, "<PageUp>"), (13, 22, 4));
    assert_eq!(at(31, 35, "2<PageUp>"), (15, 24, 4));
    // Short of a page from the text's start.
    assert_eq!(at(4, 8, "<PageUp>"), (1, 10, 4));
    // The text's end in view: a whole view, to its last line.
    assert_eq!(at(51, 55, "<C-f>"), (60, 60, 4));
    let short = viewed("a\nb\nc\n", 1, (1, 0), false);
    assert_eq!(after(short, "<C-f>"), (3, 3, 0));
    // With `scrolloff`, that many rows in.
    let margin = with_margin(viewed(&text, 45, (50, 4), false), 3);
    assert_eq!(after(margin, "<C-f>"), (53, 56, 4));
}

#[test]
fn a_page_that_cant_move_the_view_leaves_the_cursor_be_startofline_or_not() {
    let text = sixty();
    assert_eq!(
        after(viewed(&text, 1, (5, 4), false), "<PageUp>"),
        (1, 5, 4)
    );
    let mut setup = viewed(&text, 1, (6, 4), false);
    setup.set(|options| options.startofline = true);
    assert_eq!(after(setup, "<PageUp>"), (1, 6, 4));
}

#[test]
fn where_lines_wrap_paging_goes_by_screen_rows() {
    let text = wrapping();
    // Five rows down: the cursor from line 14 onto line 17's second row.
    assert_eq!(
        after(viewed(&text, 11, (14, 2), true), "<C-d>"),
        (14, 17, 22)
    );
    assert_eq!(
        after(viewed(&text, 11, (14, 2), true), "<C-f>"),
        (16, 16, 2)
    );
}

#[test]
fn where_lines_wrap_a_page_goes_to_a_lines_start() {
    let text = wrapping();
    // Eight rows from line 12 end on line 17's second: back to its first.
    // Neovim puts the cursor at line 17's end, having passed it there;
    // clackatoo keeps the column it wanted.
    assert_eq!(
        after(viewed(&text, 12, (13, 2), true), "<C-f>"),
        (17, 17, 2)
    );
    // Up from line 21, eight rows end on line 15's second: on to 16.
    assert_eq!(
        after(viewed(&text, 21, (24, 2), true), "<PageUp>"),
        (16, 22, 2)
    );
    assert_eq!(
        after(viewed(&text, 30, (30, 2), true), "<PageUp>"),
        (25, 30, 2)
    );
}

/// `viewed`, with `scrolloff` at `margin`.
fn with_margin(mut setup: Setup, margin: u32) -> Setup {
    setup.set(|options| options.scrolloff = margin);
    setup
}

#[test]
fn h_m_and_l_go_to_lines_of_the_view_by_count() {
    let text = sixty();
    let at = |keys| after(viewed(&text, 11, (15, 4), false), keys);
    assert_eq!(at("H"), (11, 11, 4));
    assert_eq!(at("3H"), (11, 13, 4));
    assert_eq!(at("M"), (11, 15, 4));
    assert_eq!(at("L"), (11, 20, 4));
    assert_eq!(at("3L"), (11, 18, 4));
    // The middle of what's shown, where the text ends in view.
    let short = viewed("a\nb\nc\nd\n", 1, (1, 0), false);
    assert_eq!(after(short, "M"), (1, 2, 0));
}

#[test]
fn h_and_l_keep_scrolloff_rows_from_the_views_edges_but_not_at_the_texts_start() {
    let text = sixty();
    let at = |keys| after(with_margin(viewed(&text, 11, (15, 4), false), 3), keys);
    assert_eq!(at("H"), (11, 14, 4));
    assert_eq!(at("2H"), (11, 14, 4));
    assert_eq!(at("5H"), (11, 15, 4));
    assert_eq!(at("L"), (11, 17, 4));
    assert_eq!(at("99H"), (11, 17, 4));
    let top = with_margin(viewed(&text, 1, (5, 4), false), 3);
    assert_eq!(after(top, "H"), (1, 1, 4));
}

#[test]
fn with_scrolloff_past_half_the_view_h_m_and_l_meet_where_vims_do() {
    let text = sixty();
    let at = |keys, margin| after(with_margin(viewed(&text, 11, (15, 4), false), margin), keys);
    // Four rows from each edge of ten leave lines 15 and 16.
    assert_eq!(at("H", 4), (11, 15, 4));
    assert_eq!(at("L", 4), (11, 16, 4));
    // Five meet at line 15, taking from the bottom first.
    for margin in [5, 10] {
        assert_eq!(at("H", margin), (11, 15, 4), "so={margin}");
        assert_eq!(at("M", margin), (11, 15, 4), "so={margin}");
        assert_eq!(at("L", margin), (11, 15, 4), "so={margin}");
    }
}

#[test]
fn h_m_and_l_keep_the_column_unless_startofline() {
    let text = sixty();
    let mut setup = viewed(&text, 12, (15, 4), false);
    setup.set(|options| options.startofline = true);
    assert_eq!(after(setup, "H"), (12, 12, 2));
}

#[test]
fn an_operator_takes_whole_lines_to_the_views_edge_scrolloff_or_not() {
    let text = sixty();
    let mut setup = with_margin(viewed(&text, 11, (15, 4), false), 3);
    setup.keys("dL");
    assert_eq!(
        setup.register().text,
        "line 15\n  line 16\nline 17\n  line 18\nline 19\n  line 20\n"
    );
    assert!(setup.register().lines);
    assert_eq!(setup.cursor(), (14, 4));
    let mut setup = viewed(&text, 11, (15, 4), false);
    setup.keys("yH");
    assert_eq!(
        setup.register().text,
        "line 11\n  line 12\nline 13\n  line 14\nline 15\n"
    );
    assert_eq!(setup.cursor(), (10, 4));
}

#[test]
fn an_operator_takes_a_count_past_the_view_as_far_as_the_text_goes() {
    let text = sixty();
    let lines = |range: std::ops::RangeInclusive<usize>| {
        (text.lines().skip(range.start() - 1))
            .take(range.count())
            .fold(String::new(), |mut taken, line| {
                let _ = writeln!(taken, "{line}");
                taken
            })
    };
    let mut setup = viewed(&text, 11, (15, 4), false);
    setup.keys("d99L");
    assert_eq!(setup.register().text, lines(1..=15));
    assert_eq!(setup.cursor(), (0, 4));
    let mut setup = viewed(&text, 11, (15, 4), false);
    setup.keys("d99H");
    assert_eq!(setup.register().text, lines(15..=60));
    assert_eq!(setup.cursor(), (13, 4));
}

#[test]
fn where_lines_wrap_h_and_l_count_lines_and_scrolloff_screen_rows() {
    let text = wrapping();
    let at = |keys, margin| after(with_margin(viewed(&text, 11, (14, 2), true), margin), keys);
    assert_eq!(at("H", 0), (11, 11, 2));
    assert_eq!(at("M", 0), (11, 13, 2));
    // Line 17 goes on past the view's bottom: line 16 is the last shown
    // whole.
    assert_eq!(at("L", 0), (11, 16, 2));
    assert_eq!(at("3H", 0), (11, 13, 2));
    assert_eq!(at("3L", 0), (11, 14, 2));
    assert_eq!(at("H", 2), (11, 12, 2));
    assert_eq!(at("L", 2), (11, 14, 2));
}

#[test]
fn h_counts_from_the_first_line_shown_whole() {
    let text = wrapping();
    // Ctrl+E leaves line 11's second row at the top: H goes to line 12,
    // not back up to show line 11 whole. Neovim's, with `smoothscroll`,
    // goes to line 11 and scrolls back.
    let at = |keys| after(viewed(&text, 11, (14, 2), true), keys);
    assert_eq!(at("<C-e>H"), (11, 12, 2));
    assert_eq!(at("<C-e>2H"), (11, 13, 2));
    assert_eq!(at("<C-e>99L"), (11, 12, 2));
}

#[test]
fn ctrl_e_and_ctrl_y_move_the_view_keeping_the_cursor_in_it() {
    let text = sixty();
    assert_eq!(
        after(viewed(&text, 11, (11, 4), false), "<C-e>"),
        (12, 12, 4)
    );
    assert_eq!(
        after(viewed(&text, 11, (11, 4), false), "3<C-e>"),
        (14, 14, 4)
    );
    assert_eq!(
        after(viewed(&text, 11, (20, 4), false), "<C-y>"),
        (10, 19, 4)
    );
    let setup = with_margin(viewed(&text, 11, (14, 4), false), 3);
    assert_eq!(after(setup, "<C-e>"), (12, 15, 4));
}

#[test]
fn pages_land_at_the_first_non_blank_with_startofline_but_ctrl_e_doesnt() {
    let text = sixty();
    let sol = |top, at, keys| {
        let mut setup = viewed(&text, top, at, false);
        setup.set(|options| options.startofline = true);
        after(setup, keys)
    };
    assert_eq!(sol(11, (15, 4), "<C-d>"), (16, 20, 2));
    assert_eq!(sol(21, (25, 4), "<C-u>"), (16, 20, 2));
    assert_eq!(sol(11, (15, 4), "<C-f>"), (19, 19, 0));
    assert_eq!(sol(21, (25, 4), "<PageUp>"), (13, 22, 2));
    assert_eq!(sol(11, (11, 4), "<C-e>"), (12, 12, 4));
}
