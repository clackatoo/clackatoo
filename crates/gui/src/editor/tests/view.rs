//! The keys that go by the view: paging (Ctrl+D, Ctrl+U, Ctrl+F, Ctrl+B,
//! Page Down and Page Up), each case checked against Neovim 0.11 headless
//! (`crates/gui/tests/nvim/check.lua`), in a view of ten rows.

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
}

#[test]
fn pages_move_the_view_less_two_rows_keeping_the_cursor_in_it() {
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
