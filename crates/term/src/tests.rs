//! Behaviour tests for the whole emulator: bytes in, screen out.

use proptest::prelude::*;

use crate::{Attributes, CellFlags, Color, Config, Size, Style, Terminal};

fn terminal(cols: u16, rows: u16) -> Terminal {
    let size = Size::new(cols, rows).expect("test sizes are valid");
    Terminal::new(
        size,
        &Config {
            scrollback_lines: 100,
        },
    )
}

/// A terminal of `cols` × `rows` after `input`, with its invariants checked.
fn after(cols: u16, rows: u16, input: &str) -> Terminal {
    let mut terminal = terminal(cols, rows);
    terminal.advance(input.as_bytes());
    terminal.assert_invariants();
    terminal
}

fn cursor(terminal: &Terminal) -> (usize, usize) {
    let cursor = terminal.cursor();
    (cursor.line, cursor.col)
}

fn style_at(terminal: &Terminal, line: usize, col: usize) -> Style {
    let row = terminal.grid().row(line);
    *row.style(row.cells()[col])
}

fn scrollback_text(terminal: &Terminal) -> Vec<String> {
    terminal.grid().scrollback().map(|row| row.text()).collect()
}

fn red_fg() -> Style {
    Style {
        fg: Color::Indexed(1),
        ..Style::DEFAULT
    }
}

fn red_bg() -> Style {
    Style {
        bg: Color::Indexed(1),
        ..Style::DEFAULT
    }
}

mod printing {
    use super::*;

    #[test]
    fn a_character_said_to_be_three_columns_wide_is_drawn_two_wide() {
        // unicode-width gives U+17D8 a width of 3; cells hold at most 2.
        let mut t = terminal(10, 2);
        t.advance("\u{17D8}x".as_bytes());
        assert_eq!(t.screen_text(), "\u{17D8}x");
        assert_eq!(cursor(&t), (0, 3));
        t.assert_invariants();
    }

    #[test]
    fn text_and_newlines() {
        let t = after(10, 3, "hello\r\nworld");
        assert_eq!(t.screen_text(), "hello\nworld");
        assert_eq!(cursor(&t), (1, 5));
    }

    #[test]
    fn line_feed_alone_keeps_the_column() {
        let t = after(10, 3, "ab\ncd");
        assert_eq!(t.screen_text(), "ab\n  cd");
    }

    #[test]
    fn filling_a_line_defers_the_wrap() {
        let t = after(4, 3, "abcd");
        assert_eq!(t.screen_text(), "abcd");
        assert_eq!(cursor(&t), (0, 3), "cursor stays on the last column");
        assert!(!t.grid().row(0).is_wrapped());

        let t = after(4, 3, "abcde");
        assert_eq!(t.screen_text(), "abcd\ne");
        assert!(t.grid().row(0).is_wrapped());
    }

    #[test]
    fn a_pending_wrap_is_cancelled_by_cursor_movement() {
        let t = after(4, 3, "abcd\rX");
        assert_eq!(t.screen_text(), "Xbcd");

        // Exactly filling a line then CR LF must not leave a blank line.
        let t = after(4, 3, "abcd\r\nef");
        assert_eq!(t.screen_text(), "abcd\nef");
    }

    #[test]
    fn without_autowrap_the_last_column_is_overwritten() {
        let t = after(4, 3, "\x1b[?7labcdef");
        assert_eq!(t.screen_text(), "abcf");
        assert_eq!(cursor(&t), (0, 3));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let t = after(6, 2, "a字b");
        assert_eq!(t.screen_text(), "a字b");
        assert_eq!(cursor(&t), (0, 4));
    }

    #[test]
    fn a_wide_character_that_does_not_fit_wraps_first() {
        let t = after(4, 2, "abc字");
        assert_eq!(t.screen_text(), "abc\n字");
        assert!(t.grid().row(0).is_wrapped());
        assert_eq!(cursor(&t), (1, 2));
    }

    #[test]
    fn combining_marks_join_the_previous_character() {
        let t = after(6, 2, "e\u{301}x");
        assert_eq!(t.screen_text(), "e\u{301}x");
        assert_eq!(cursor(&t), (0, 2));
    }

    #[test]
    fn combining_mark_after_a_pending_wrap_joins_the_last_column() {
        let t = after(3, 2, "abc\u{301}");
        assert_eq!(t.screen_text(), "abc\u{301}");
    }

    #[test]
    fn zwj_sequences_form_one_wide_cell() {
        let t = after(6, 2, "👩\u{200D}💻x");
        assert_eq!(t.screen_text(), "👩\u{200D}💻x");
        assert_eq!(cursor(&t), (0, 3));
    }

    #[test]
    fn insert_mode_shifts_the_line() {
        let t = after(6, 2, "abc\r\x1b[4hX\x1b[4lY");
        assert_eq!(t.screen_text(), "XYbc");
    }

    #[test]
    fn repeat_prints_the_last_character_again() {
        let t = after(10, 2, "ab\x1b[3b");
        assert_eq!(t.screen_text(), "abbbb");
    }

    #[test]
    fn printed_characters_take_the_pen_style() {
        let t = after(10, 2, "\x1b[31mA\x1b[0mB");
        assert_eq!(style_at(&t, 0, 0), red_fg());
        assert_eq!(style_at(&t, 0, 1), Style::DEFAULT);
    }

    #[test]
    fn dec_special_graphics_draws_boxes() {
        let t = after(10, 2, "\x1b(0lqk\x1b(Bq");
        assert_eq!(t.screen_text(), "┌─┐q");
    }

    #[test]
    fn shift_out_selects_g1() {
        let t = after(10, 2, "\x1b)0q\x0eq\x0fq");
        assert_eq!(t.screen_text(), "q─q");
    }

    #[test]
    fn tabs_move_to_stops() {
        let t = after(20, 2, "a\tb\x1b[2Zc");
        assert_eq!(t.screen_text(), "c       b");

        let t = after(20, 2, "\x1b[3g\x1b[5G\x1bH\ra\tb");
        assert_eq!(t.screen_text(), "a   b");
    }

    #[test]
    fn a_character_split_between_reads_loses_nothing() {
        // vte, the parser clackatoo used to use, dropped the `e` here.
        let mut t = terminal(10, 2);
        t.advance(b"\xcc");
        t.advance(b"\x81e\xcc\x81");
        assert_eq!(t.screen_text(), "e\u{301}");
    }

    #[test]
    fn invalid_utf8_prints_a_replacement_character() {
        let mut t = terminal(10, 2);
        t.advance(b"a\xffb");
        assert_eq!(t.screen_text(), "a\u{FFFD}b");
    }
}

mod scrolling {
    use super::*;

    #[test]
    fn output_past_the_bottom_scrolls_into_scrollback() {
        let t = after(5, 2, "1\r\n2\r\n3\r\n4");
        assert_eq!(t.screen_text(), "3\n4");
        assert_eq!(scrollback_text(&t), ["1", "2"]);
    }

    #[test]
    fn a_scroll_region_scrolls_alone_and_keeps_no_history() {
        let t = after(5, 4, "a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[3;1H\nX");
        assert_eq!(t.screen_text(), "a\nc\nX\nd");
        assert!(scrollback_text(&t).is_empty());
    }

    #[test]
    fn reverse_index_at_the_top_scrolls_down() {
        let t = after(5, 3, "a\r\nb\x1b[H\x1bMX");
        assert_eq!(t.screen_text(), "X\na\nb");
    }

    #[test]
    fn scroll_up_and_down_sequences() {
        let t = after(5, 3, "a\r\nb\r\nc\x1b[1S");
        assert_eq!(t.screen_text(), "b\nc");

        let t = after(5, 3, "a\r\nb\r\nc\x1b[1T");
        assert_eq!(t.screen_text(), "\na\nb");
    }

    #[test]
    fn insert_and_delete_lines_stay_inside_the_region() {
        let t = after(5, 4, "a\r\nb\r\nc\r\nd\x1b[1;3r\x1b[2;1H\x1b[L");
        assert_eq!(t.screen_text(), "a\n\nb\nd");

        let t = after(5, 4, "a\r\nb\r\nc\r\nd\x1b[1;3r\x1b[1;1H\x1b[M");
        assert_eq!(t.screen_text(), "b\nc\n\nd");
    }

    #[test]
    fn invalid_scroll_regions_are_ignored() {
        let t = after(5, 4, "\x1b[3;3r\x1b[4;1r");
        t.assert_invariants();
    }
}

mod cursor_movement {
    use super::*;

    #[test]
    fn absolute_and_relative_moves_are_clamped() {
        assert_eq!(cursor(&after(10, 5, "\x1b[3;4H")), (2, 3));
        assert_eq!(cursor(&after(10, 5, "\x1b[99;99H")), (4, 9));
        assert_eq!(cursor(&after(10, 5, "\x1b[3;4H\x1b[2A\x1b[3D")), (0, 0));
        assert_eq!(cursor(&after(10, 5, "\x1b[3;4H\x1bB\x1b[2C")), (2, 5));
        assert_eq!(
            cursor(&after(10, 5, "\x1b[3;4H\x1b[0B")),
            (3, 3),
            "0 means 1"
        );
        assert_eq!(cursor(&after(10, 5, "\x1b[5G\x1b[4d")), (3, 4));
    }

    #[test]
    fn vertical_moves_stop_at_the_scroll_region() {
        assert_eq!(cursor(&after(10, 6, "\x1b[2;4r\x1b[3;1H\x1b[9A")), (1, 0));
        assert_eq!(cursor(&after(10, 6, "\x1b[2;4r\x1b[3;1H\x1b[9B")), (3, 0));
        // Starting outside the region, the screen edge is the limit.
        assert_eq!(cursor(&after(10, 6, "\x1b[2;4r\x1b[6;1H\x1b[9A")), (1, 0));
        assert_eq!(cursor(&after(10, 6, "\x1b[2;4r\x1b[5;1H\x1b[9B")), (5, 0));
    }

    #[test]
    fn origin_mode_addresses_relative_to_the_region() {
        let t = after(10, 6, "\x1b[2;4r\x1b[?6h\x1b[2;3HX");
        assert_eq!(cursor(&t), (2, 3));
        assert_eq!(after(10, 6, "\x1b[2;4r\x1b[?6h\x1b[9;1H").cursor().line, 3);
    }

    #[test]
    fn save_and_restore_cursor() {
        let t = after(10, 4, "\x1b[2;3H\x1b[31m\x1b7\x1b[H\x1b[0m\x1b8X");
        assert_eq!(cursor(&t), (1, 3));
        assert_eq!(style_at(&t, 1, 2), red_fg());

        let t = after(10, 4, "\x1b[2;3H\x1b[s\x1b[H\x1b[uX");
        assert_eq!(cursor(&t), (1, 3));
    }

    #[test]
    fn backspace_stops_at_the_left_edge() {
        assert_eq!(cursor(&after(10, 2, "ab\x08\x08\x08")), (0, 0));
    }
}

mod erasing {
    use super::*;

    #[test]
    fn erase_in_display() {
        let screen = "abc\r\ndef\r\nghi\x1b[2;2H";
        assert_eq!(
            after(5, 3, &format!("{screen}\x1b[J")).screen_text(),
            "abc\nd"
        );
        assert_eq!(
            after(5, 3, &format!("{screen}\x1b[1J")).screen_text(),
            "\n  f\nghi"
        );
        assert_eq!(after(5, 3, &format!("{screen}\x1b[2J")).screen_text(), "");
    }

    #[test]
    fn erase_scrollback() {
        let t = after(5, 2, "1\r\n2\r\n3\x1b[3J");
        assert!(scrollback_text(&t).is_empty());
        assert_eq!(t.screen_text(), "2\n3");
    }

    #[test]
    fn erase_in_line() {
        let line = "abcde\x1b[3G";
        assert_eq!(after(5, 1, &format!("{line}\x1b[K")).screen_text(), "ab");
        assert_eq!(
            after(5, 1, &format!("{line}\x1b[1K")).screen_text(),
            "   de"
        );
        assert_eq!(after(5, 1, &format!("{line}\x1b[2K")).screen_text(), "");
    }

    #[test]
    fn erasing_uses_the_current_background() {
        let t = after(5, 2, "\x1b[41;1m\x1b[2J");
        assert_eq!(style_at(&t, 1, 4), red_bg(), "only the background is kept");
    }

    #[test]
    fn character_editing() {
        assert_eq!(after(6, 1, "abcdef\x1b[2G\x1b[2X").screen_text(), "a  def");
        assert_eq!(after(6, 1, "abcdef\x1b[2G\x1b[2P").screen_text(), "adef");
        assert_eq!(after(6, 1, "abcdef\x1b[2G\x1b[2@").screen_text(), "a  bcd");
    }

    #[test]
    fn screen_alignment_test_fills_with_e() {
        assert_eq!(after(3, 2, "\x1b#8").screen_text(), "EEE\nEEE");
    }
}

mod screens {
    use super::*;

    #[test]
    fn alternate_screen_preserves_the_primary_screen_and_cursor() {
        let mut t = after(10, 3, "shell$ vim\x1b[?1049h\x1b[Hediting");
        assert!(t.is_alternate_screen());
        assert_eq!(t.screen_text(), "editing");

        t.advance(b"\x1b[?1049l");
        t.assert_invariants();
        assert!(!t.is_alternate_screen());
        assert_eq!(t.screen_text(), "shell$ vim");
        assert_eq!(cursor(&t), (0, 9));
    }

    #[test]
    fn the_alternate_screen_starts_clear_each_time() {
        let t = after(10, 3, "\x1b[?1049hjunk\x1b[?1049l\x1b[?1049h");
        assert_eq!(t.screen_text(), "");
    }

    #[test]
    fn the_alternate_screen_keeps_no_scrollback() {
        let t = after(5, 2, "\x1b[?1049h1\r\n2\r\n3");
        assert_eq!(t.screen_text(), "2\n3");
        assert!(scrollback_text(&t).is_empty());
    }

    #[test]
    fn soft_reset_restores_modes_but_not_the_screen() {
        let t = after(5, 3, "abc\x1b[?7l\x1b[31m\x1b[2;3r\x1b[!p\x1b[1;5Hdefg");
        // Autowrap is back on, and the default pen.
        assert_eq!(t.screen_text(), "abc d\nefg");
        assert_eq!(style_at(&t, 0, 4), Style::DEFAULT);
    }

    #[test]
    fn full_reset_clears_everything() {
        let t = after(5, 2, "\x1b]2;title\x07\x1b[31m1\r\n2\r\n3\x1bc");
        assert_eq!(t.screen_text(), "");
        assert!(scrollback_text(&t).is_empty());
        assert_eq!(t.title(), "");
        assert_eq!(cursor(&t), (0, 0));
    }
}

mod queries {
    use super::*;

    fn replies(input: &str) -> String {
        let mut t = after(10, 5, input);
        String::from_utf8(t.take_replies()).expect("replies are ASCII")
    }

    #[test]
    fn primary_device_attributes() {
        assert_eq!(replies("\x1b[c"), "\x1b[?62;22c");
        assert_eq!(replies("\x1b[0c"), "\x1b[?62;22c");
    }

    #[test]
    fn device_status_reports() {
        assert_eq!(replies("\x1b[5n"), "\x1b[0n");
        assert_eq!(replies("\x1b[3;7H\x1b[6n"), "\x1b[3;7R");
        assert_eq!(replies("\x1b[3;7H\x1b[?6n"), "\x1b[?3;7R");
        assert_eq!(replies("\x1b[2;4r\x1b[?6h\x1b[2;1H\x1b[6n"), "\x1b[2;1R");
    }

    #[test]
    fn version() {
        assert_eq!(
            replies("\x1b[>q"),
            format!("\x1bP>|clackatoo {}\x1b\\", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn replies_are_taken_once() {
        let mut t = after(10, 5, "\x1b[c");
        assert!(!t.take_replies().is_empty());
        assert!(t.take_replies().is_empty());
    }

    #[test]
    fn unsupported_queries_get_no_reply() {
        // The kitty keyboard query: fish follows it with DA1 and treats a
        // DA1-only answer as "unsupported".
        assert_eq!(replies("\x1b[?u\x1b[c"), "\x1b[?62;22c");
    }
}

mod other_sequences {
    use super::*;

    #[test]
    fn osc_sets_the_title() {
        assert_eq!(
            after(5, 1, "\x1b]2;hello; world\x07").title(),
            "hello; world"
        );
        assert_eq!(after(5, 1, "\x1b]0;both\x1b\\").title(), "both");
    }

    #[test]
    fn hiding_the_cursor() {
        assert!(!after(5, 1, "\x1b[?25l").cursor().visible);
        assert!(after(5, 1, "\x1b[?25l\x1b[?25h").cursor().visible);
    }

    #[test]
    fn unknown_sequences_print_nothing() {
        let t = after(
            20,
            2,
            "a\x1b[?1000h\x1b[>4;1m\x1bP+q544e\x1b\\\x1b]133;A\x07\x1b[5 qb",
        );
        assert_eq!(t.screen_text(), "ab");
    }

    #[test]
    fn sgr_attributes_reach_the_grid() {
        let t = after(5, 1, "\x1b[1;4:3;38;2;1;2;3mX");
        let style = style_at(&t, 0, 0);
        assert!(style.attributes.contains(Attributes::BOLD));
        assert_eq!(style.underline, crate::Underline::Curly);
        assert_eq!(style.fg, Color::Rgb(1, 2, 3));
    }
}

/// Pieces of terminal output, from plain text to the escape sequences the
/// emulator handles. Concatenated at random, they exercise far more state
/// combinations than hand-written tests.
fn output_fragment() -> impl Strategy<Value = Vec<u8>> {
    let printable = prop_oneof![
        "[a-z ]{1,12}",
        Just("字".to_owned()),
        Just("e\u{301}".to_owned()),
        Just("👩\u{200D}💻".to_owned()),
        Just("\u{301}".to_owned()),
        Just("\u{200D}".to_owned()),
        Just("αβ😀".to_owned()),
    ]
    .prop_map(String::into_bytes);
    let control = prop::sample::select(vec![
        b"\r".to_vec(),
        b"\n".to_vec(),
        b"\x08".to_vec(),
        b"\t".to_vec(),
        b"\x0e".to_vec(),
        b"\x0f".to_vec(),
    ]);
    let csi = (
        prop::sample::select(vec!["", "?", ">", "!"]),
        prop::collection::vec(0u16..30, 0..3),
        prop::sample::select(
            "@ABCDEFGHIJKLMPSTXZabcdeghlmnrsu"
                .chars()
                .collect::<Vec<_>>(),
        ),
    )
        .prop_map(|(intermediate, params, action)| {
            let params: Vec<String> = params.iter().map(u16::to_string).collect();
            format!("\x1b[{intermediate}{}{action}", params.join(";")).into_bytes()
        });
    let modes = (
        prop::sample::select(vec![4u16, 6, 7, 20, 25, 47, 1047, 1048, 1049]),
        any::<bool>(),
    )
        .prop_map(|(mode, on)| {
            let private = if mode == 4 || mode == 20 { "" } else { "?" };
            format!("\x1b[{private}{mode}{}", if on { 'h' } else { 'l' }).into_bytes()
        });
    let esc = prop::sample::select(vec![
        b"\x1b7".to_vec(),
        b"\x1b8".to_vec(),
        b"\x1bD".to_vec(),
        b"\x1bE".to_vec(),
        b"\x1bH".to_vec(),
        b"\x1bM".to_vec(),
        b"\x1bc".to_vec(),
        b"\x1b#8".to_vec(),
        b"\x1b(0".to_vec(),
        b"\x1b(B".to_vec(),
        b"\x1b]2;title\x07".to_vec(),
    ]);
    prop_oneof![4 => printable, 2 => control, 3 => csi, 1 => modes, 1 => esc, 1 => any::<u8>().prop_map(|b| vec![b])]
}

fn output() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(output_fragment(), 0..60).prop_map(|fragments| fragments.concat())
}

/// Everything observable about a terminal, for comparing two of them.
fn observe(
    terminal: &mut Terminal,
) -> (String, Vec<String>, (usize, usize), bool, String, Vec<u8>) {
    (
        terminal.screen_text(),
        scrollback_text(terminal),
        cursor(terminal),
        terminal.is_alternate_screen(),
        terminal.title().to_owned(),
        terminal.take_replies(),
    )
}

/// One row, cell by cell: each cell's text, flags and style, plus whether the
/// row wraps.
type RowContents = (Vec<(String, CellFlags, Style)>, bool);

fn row_contents(row: &crate::Row) -> RowContents {
    let cells = (0..row.width())
        .map(|col| {
            let mut text = String::new();
            row.push_text(col, &mut text);
            let cell = row.cells()[col];
            (text, cell.flags(), *row.style(cell))
        })
        .collect();
    (cells, row.is_wrapped())
}

/// Every row of both screen and scrollback, in full.
fn cells(terminal: &Terminal) -> Vec<RowContents> {
    let grid = terminal.grid();
    grid.scrollback()
        .map(|row| row_contents(&row))
        .chain(grid.rows().map(row_contents))
        .collect()
}

proptest! {
    #[test]
    fn scrollback_stores_any_row_exactly(bytes in output(), cols in 1u16..12, rows in 1u16..6) {
        let mut t = terminal(cols, rows);
        t.advance(&bytes);
        for row in t.grid().rows() {
            let mut history = crate::scrollback::Scrollback::new(1);
            history.push(row, false);
            let stored = history.rows().next().expect("one row was stored");
            prop_assert_eq!(row_contents(&stored), row_contents(row));
        }
    }

    #[test]
    fn the_fast_print_path_matches_the_simple_one(
        bytes in output(),
        cols in 1u16..12,
        rows in 1u16..6,
    ) {
        let mut fast = terminal(cols, rows);
        let mut simple = terminal(cols, rows);
        simple.use_simple_print();
        fast.advance(&bytes);
        simple.advance(&bytes);
        prop_assert_eq!(cells(&fast), cells(&simple));
        prop_assert_eq!(fast.emulator_state(), simple.emulator_state());
    }

    #[test]
    fn any_output_keeps_the_invariants(bytes in output(), cols in 1u16..12, rows in 1u16..6) {
        let mut t = terminal(cols, rows);
        t.advance(&bytes);
        t.assert_invariants();
    }

    #[test]
    fn splitting_output_never_changes_the_result(
        bytes in output(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 1..6),
    ) {
        let mut whole = terminal(8, 4);
        whole.advance(&bytes);

        let mut cuts: Vec<usize> = cuts.iter().map(|cut| cut.index(bytes.len() + 1)).collect();
        cuts.sort_unstable();
        let mut pieces = terminal(8, 4);
        let mut start = 0;
        for cut in cuts.into_iter().chain([bytes.len()]) {
            pieces.advance(&bytes[start..cut]);
            start = cut;
        }

        prop_assert_eq!(observe(&mut whole), observe(&mut pieces));
    }
}
