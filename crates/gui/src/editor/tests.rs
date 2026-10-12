//! An editor driven by keys, with its buffer's copy talking to a daemon as
//! the real one does.

use clackatoo_protocol::{BufferId, Size};

use super::copy::tests::Daemon;
use super::keys::Stroke;
use super::{BufferCopy, Editor, Mode, Register};
use crate::options::Options;
use crate::pane::Selection;
use crate::render::HighlightKind;

struct Setup {
    editor: Editor,
    copy: BufferCopy,
    daemon: Daemon,
    register: Register,
    options: Options,
}

impl Setup {
    fn new(text: &str) -> Self {
        let daemon = Daemon::new(text);
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        let options = Options::default();
        let mut editor = Editor::new(BufferId(1), &options);
        editor.resize(Size::new(40, 10).unwrap());
        Self {
            editor,
            copy,
            daemon,
            register: Register::default(),
            options,
        }
    }

    /// Changes the options, as `:set` does.
    fn set(&mut self, change: impl FnOnce(&mut Options)) {
        change(&mut self.options);
        self.editor.set_options(&self.options, &self.copy);
    }

    /// The first line shown, and the cursor's line and column.
    fn view(&self) -> (u64, u64, usize) {
        let (line, col) = self.cursor();
        (self.editor.top, line, col)
    }

    /// The highlights of `kind`, as lines and columns.
    fn highlighted(&mut self, kind: HighlightKind) -> Vec<(usize, std::ops::Range<usize>)> {
        self.editor.settle(&self.copy);
        (self.editor.highlights().into_iter())
            .filter(|highlight| highlight.kind == kind)
            .map(|highlight| (highlight.line, highlight.cols))
            .collect()
    }

    /// Types `keys`, Vim's way: `<Esc>`, `<CR>`, `<BS>`, `<Tab>`, `<C-r>`,
    /// and `<lt>` for `<`.
    fn keys(&mut self, keys: &str) {
        for stroke in strokes(keys) {
            self.editor
                .stroke(stroke, &mut self.copy, &mut self.register);
            self.serve();
        }
    }

    /// Passes the window's changes to the daemon, and its answers back.
    fn serve(&mut self) {
        while let Some(message) = self.copy.take_outbox().into_iter().next() {
            let (answer, _) = self.daemon.take(message);
            self.editor.hold(&self.copy);
            let heard = self.copy.update(answer);
            self.editor.heard(&heard, true, &self.copy);
        }
    }

    fn text(&self) -> String {
        let text = self.copy.text().to_string();
        assert_eq!(
            text,
            self.daemon.text.to_string(),
            "the daemon has the same"
        );
        text
    }

    fn cursor(&self) -> (u64, usize) {
        (self.editor.cursor.at.line, self.editor.cursor.at.col)
    }
}

fn strokes(keys: &str) -> Vec<Stroke> {
    let mut strokes = Vec::new();
    let mut rest = keys;
    while let Some(ch) = rest.chars().next() {
        if ch == '<'
            && let Some(end) = rest.find('>')
        {
            let name = &rest[1..end];
            strokes.push(match name {
                "Esc" => Stroke::Escape,
                "CR" => Stroke::Enter,
                "BS" => Stroke::Backspace,
                "Tab" => Stroke::Tab,
                "Right" => Stroke::Right,
                "lt" => Stroke::char('<'),
                _ => match name.strip_prefix("C-") {
                    Some(key) => Stroke::ctrl(key.chars().next().unwrap()),
                    None => panic!("no key <{name}>"),
                },
            });
            rest = &rest[end + 1..];
            continue;
        }
        strokes.push(Stroke::char(ch));
        rest = &rest[ch.len_utf8()..];
    }
    strokes
}

#[test]
fn typing_in_insert_mode_and_back() {
    let mut setup = Setup::new("");
    setup.keys("ihello<Esc>");
    assert_eq!(setup.text(), "hello");
    assert_eq!(setup.editor.mode, Mode::Normal);
    assert_eq!(setup.cursor(), (0, 4));
    setup.keys("a world<CR>again<Esc>");
    assert_eq!(setup.text(), "hello world\nagain");
    setup.keys("0iX<BS>Y<Esc>");
    assert_eq!(setup.text(), "hello world\nYagain");
    // Enter at the end of a file that ends with a line break.
    let mut setup = Setup::new("a\n");
    setup.keys("A<CR>b<Esc>");
    assert_eq!(setup.text(), "a\nb\n");
    assert_eq!(setup.cursor(), (1, 0));
}

#[test]
fn an_insert_is_one_undo_step_and_redo_brings_it_back() {
    let mut setup = Setup::new("start\n");
    setup.keys("Atyped<Esc>");
    setup.keys("ox<Esc>");
    assert_eq!(setup.text(), "starttyped\nx\n");
    setup.keys("u");
    assert_eq!(setup.text(), "starttyped\n");
    setup.keys("u");
    assert_eq!(setup.text(), "start\n");
    setup.keys("u");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("already at the oldest change")
    );
    setup.keys("<C-r><C-r>");
    assert_eq!(setup.text(), "starttyped\nx\n");
}

#[test]
fn operators_act_on_motions_and_dot_repeats() {
    let mut setup = Setup::new("one two three four\n");
    setup.keys("dw");
    assert_eq!(setup.text(), "two three four\n");
    setup.keys(".");
    assert_eq!(setup.text(), "three four\n");
    setup.keys("2.");
    assert_eq!(setup.text(), "\n", "the line stays, empty");
    setup.keys("u");
    assert_eq!(setup.text(), "three four\n");
    setup.keys("de");
    assert_eq!(setup.text(), " four\n");
    setup.keys("d$");
    assert_eq!(setup.text(), "\n");
}

#[test]
fn change_word_changes_to_the_words_end() {
    let mut setup = Setup::new("foo bar baz");
    setup.keys("cwnew<Esc>");
    assert_eq!(setup.text(), "new bar baz");
    // On a word's last character, just that character.
    assert_eq!(setup.cursor(), (0, 2));
    setup.keys("cwX<Esc>");
    assert_eq!(setup.text(), "neX bar baz");
    setup.keys("w.");
    assert_eq!(setup.text(), "neX X baz");
}

#[test]
fn lines_are_deleted_copied_and_put() {
    let mut setup = Setup::new("a\nb\nc");
    setup.keys("yyp");
    assert_eq!(setup.text(), "a\na\nb\nc");
    assert_eq!(setup.cursor(), (1, 0));
    setup.keys("Gdd");
    assert_eq!(
        setup.text(),
        "a\na\nb",
        "the last line takes the line break before it"
    );
    setup.keys("ggP");
    assert_eq!(setup.text(), "c\na\na\nb");
    setup.keys("2dd");
    assert_eq!(setup.text(), "a\nb");
    setup.keys("Gp");
    assert_eq!(setup.text(), "a\nb\nc\na");
}

#[test]
fn characters_are_deleted_replaced_and_put() {
    let mut setup = Setup::new("abcdef");
    setup.keys("2x");
    assert_eq!(setup.text(), "cdef");
    setup.keys("p");
    assert_eq!(setup.text(), "cabdef");
    setup.keys("$X");
    assert_eq!(setup.text(), "cabdf");
    setup.keys("03rz");
    assert_eq!(setup.text(), "zzzdf");
    assert_eq!(setup.cursor(), (0, 2));
    setup.keys("C!<Esc>");
    assert_eq!(setup.text(), "zz!");
}

#[test]
fn objects_new_lines_counts_and_joins() {
    let mut setup = Setup::new("    let x = \"hello\";\n");
    setup.keys("fhci\"bye<Esc>");
    assert_eq!(setup.text(), "    let x = \"bye\";\n");
    setup.keys("ofoo<Esc>");
    assert_eq!(
        setup.text(),
        "    let x = \"bye\";\n    foo\n",
        "the new line keeps the indent"
    );
    setup.keys("kJ");
    assert_eq!(setup.text(), "    let x = \"bye\"; foo\n");
    setup.keys("03ix<Esc>");
    assert_eq!(setup.text(), "xxx    let x = \"bye\"; foo\n");
}

#[test]
fn visual_mode_selects_then_acts() {
    let mut setup = Setup::new("abc\ndef\n");
    setup.keys("lvjd");
    assert_eq!(setup.text(), "af\n");
    setup.keys("u");
    setup.keys("Vjy");
    assert_eq!(setup.register.text, "abc\ndef\n");
    assert!(setup.register.lines);
}

#[test]
fn search_finds_and_wraps() {
    let mut setup = Setup::new("a two\nb two\nc\n");
    setup.keys("/two<CR>");
    assert_eq!(setup.cursor(), (0, 2));
    setup.keys("n");
    assert_eq!(setup.cursor(), (1, 2));
    setup.keys("n");
    assert_eq!(setup.cursor(), (0, 2), "round past the end");
    setup.keys("N");
    assert_eq!(setup.cursor(), (1, 2));
}

#[test]
fn a_tab_is_one_character_to_move_over() {
    let mut setup = Setup::new("\tx\ty");
    setup.keys("l");
    assert_eq!(setup.cursor(), (0, 4));
    setup.keys("l");
    assert_eq!(setup.cursor(), (0, 5));
    setup.keys("x");
    assert_eq!(setup.text(), "\txy");
    setup.keys("0i<Tab><Esc>");
    assert_eq!(
        setup.text(),
        "\t\txy",
        "a file indented with tabs gets tabs"
    );
}

#[test]
fn an_agents_edit_moves_the_cursor_with_the_text() {
    let mut setup = Setup::new("hello world");
    setup.keys("w");
    assert_eq!(setup.cursor(), (0, 6));
    let update = setup.daemon.agent(0, "// a\n");
    setup.editor.hold(&setup.copy);
    let heard = setup.copy.update(update);
    setup.editor.heard(&heard, false, &setup.copy);
    assert_eq!(setup.text(), "// a\nhello world");
    assert_eq!(setup.cursor(), (1, 6));
    // Undo takes back yours, not theirs.
    setup.keys("x");
    setup.keys("u");
    assert_eq!(setup.text(), "// a\nhello world");
    assert_eq!(setup.cursor(), (1, 6));
}

#[test]
fn lines_are_numbered_from_the_cursors() {
    let mut setup = Setup::new("a\nb\nc\nd\n");
    setup.keys("jj");
    setup.editor.settle(&setup.copy);
    let (rows, cursor) = setup.editor.scene();
    let numbers: Vec<String> = rows[..4]
        .iter()
        .map(|row| row.text()[..4].to_owned())
        .collect();
    assert_eq!(numbers, ["  2 ", "  1 ", "  3 ", "  1 "]);
    assert_eq!(rows[2].text(), "  3 c");
    assert_eq!(rows[4].text(), "~", "past the end, no number");
    let cursor = cursor.unwrap();
    assert_eq!((cursor.line, cursor.col), (2, 4), "right of the numbers");
    let fg = |row: &clackatoo_term::Row| row.style(row.cells()[2]).fg;
    assert_eq!(fg(rows[2]), crate::colors::line_number(true).fg);
    assert_eq!(fg(rows[1]), crate::colors::line_number(false).fg);
}

#[test]
fn the_numbers_widen_with_the_file_and_go_in_a_narrow_pane() {
    let text = "x\n".repeat(12_345);
    let mut setup = Setup::new(&text);
    setup.keys("G");
    setup.editor.settle(&setup.copy);
    let (rows, cursor) = setup.editor.scene();
    assert!(rows.iter().any(|row| row.text() == "12345 x"));
    assert_eq!(cursor.unwrap().col, 6);

    setup.editor.resize(Size::new(17, 10).unwrap());
    setup.editor.settle(&setup.copy);
    let (rows, cursor) = setup.editor.scene();
    assert!(
        rows.iter()
            .all(|row| row.text() == "x" || row.text() == "~")
    );
    assert_eq!(cursor.unwrap().col, 0);
}

#[test]
fn clicks_and_selections_are_beside_the_numbers() {
    let mut setup = Setup::new("hello\nworld\n");
    setup.editor.settle(&setup.copy);
    setup.editor.click(1, 6, &setup.copy);
    assert_eq!(setup.cursor(), (1, 2));
    setup.editor.click(0, 1, &setup.copy);
    assert_eq!(setup.cursor(), (0, 0), "on a number, the line's start");
    setup.keys("vl");
    let selected = (setup.editor.highlights().into_iter())
        .find(|highlight| highlight.kind == HighlightKind::Selection)
        .unwrap();
    assert_eq!(selected.cols, 4..6);
}

#[test]
fn the_options_choose_the_numbers() {
    let mut setup = Setup::new("a\nb\nc\n");
    setup.keys("j");
    let mut shown = |options: &Options| {
        setup.editor.set_options(options, &setup.copy);
        setup.editor.settle(&setup.copy);
        let (rows, _) = setup.editor.scene();
        rows[..3].iter().map(|row| row.text()).collect::<Vec<_>>()
    };
    let mut options = Options {
        relativenumber: false,
        ..Options::default()
    };
    assert_eq!(shown(&options), ["  1 a", "  2 b", "  3 c"]);
    options.number = false;
    assert_eq!(shown(&options), ["a", "b", "c"]);
    options.relativenumber = true;
    options.numberwidth = 3;
    assert_eq!(shown(&options), [" 1 a", " 0 b", " 1 c"]);
}

#[test]
fn the_line_typed_on_after_the_last_is_numbered_in_the_gutter() {
    // 999 lines, then Enter at the end of the last: line 1000.
    let mut setup = Setup::new(&format!("{}x", "x\n".repeat(998)));
    setup.keys("GA<CR>");
    setup.editor.settle(&setup.copy);
    let (rows, cursor) = setup.editor.scene();
    let cursor = cursor.unwrap();
    assert_eq!(rows[cursor.line].text(), "1000");
    assert_eq!(cursor.col, 5, "right of the numbers, which widened");
}

#[test]
fn tab_indents_as_the_line_is_or_as_expandtab_says() {
    let mut setup = Setup::new("x\n");
    setup.keys("i<Tab><Esc>");
    assert_eq!(setup.text(), "    x\n", "to shiftwidth, which is tabstop's");
    setup.set(|options| options.shiftwidth = 2);
    // From column 5, to the next multiple of 2.
    setup.keys("A<Tab>y<Esc>");
    assert_eq!(setup.text(), "    x y\n");
    setup.set(|options| options.expandtab = false);
    setup.keys("A<Tab><Esc>");
    assert_eq!(
        setup.text(),
        "    x y \n",
        "a line indented with spaces keeps to spaces"
    );

    let mut setup = Setup::new("x\n");
    setup.set(|options| options.expandtab = false);
    setup.keys("i<Tab><Esc>");
    assert_eq!(setup.text(), "\tx\n", "a line not indented yet gets a tab");
}

#[test]
fn indent_operators_shift_whole_lines_by_shiftwidth() {
    let mut setup = Setup::new("fn a\n\n  b\nc\n");
    setup.register.text = "kept".into();
    setup.keys("w>G");
    assert_eq!(
        setup.text(),
        "    fn a\n\n      b\n    c\n",
        "every line whole, but empty lines stay empty"
    );
    assert_eq!(
        setup.cursor(),
        (0, 4),
        "on the first line's first character"
    );
    assert_eq!(setup.register.text, "kept", "the register isn't touched");
    setup.keys("u");
    assert_eq!(setup.text(), "fn a\n\n  b\nc\n", "one undo step");

    let mut setup = Setup::new("a b\nc\n");
    setup.keys(">w");
    assert_eq!(
        setup.text(),
        "    a b\nc\n",
        "a motion on one line, that line"
    );
    setup.keys(".");
    assert_eq!(setup.text(), "        a b\nc\n", "`.` does it again");
    setup.keys("<lt><lt>");
    assert_eq!(setup.text(), "    a b\nc\n");
    setup.keys("2>>");
    assert_eq!(setup.text(), "        a b\n    c\n", "a count is lines");
    setup.keys("3<lt><lt>");
    assert_eq!(setup.text(), "    a b\nc\n", "as many as there are");
    setup.keys("j<lt><lt>");
    assert_eq!(setup.text(), "    a b\nc\n", "and none past the start");
}

#[test]
fn shifting_keeps_tabs_on_a_line_indented_with_them() {
    let mut setup = Setup::new("\tx\n");
    setup.keys(">>");
    assert_eq!(setup.text(), "\t\tx\n");
    setup.set(|options| options.shiftwidth = 2);
    setup.keys("<lt><lt>");
    assert_eq!(
        setup.text(),
        "\t  x\n",
        "tabs as far as they go, then spaces"
    );
    setup.keys("<lt><lt>");
    assert_eq!(setup.text(), "\tx\n");
}

#[test]
fn visual_indent_keeps_the_selection_to_shift_again() {
    let mut setup = Setup::new("ab\ncd\nef\n");
    setup.keys("Vj>");
    assert_eq!(setup.text(), "    ab\n    cd\nef\n");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Lines));
    setup.keys("2>");
    assert_eq!(
        setup.text(),
        "            ab\n            cd\nef\n",
        "a count is steps"
    );
    setup.keys("<lt><Esc>");
    assert_eq!(setup.text(), "        ab\n        cd\nef\n");
    assert_eq!(setup.editor.mode, Mode::Normal);

    // By characters, the selection stays on the same ones.
    let mut setup = Setup::new("hello\n");
    setup.keys("lvl>");
    assert_eq!(setup.text(), "    hello\n");
    let anchor = setup.editor.anchor.map(|anchor| (anchor.line, anchor.col));
    assert_eq!((anchor, setup.cursor()), (Some((0, 5)), (0, 6)));
}

#[test]
fn tabstop_sets_how_wide_a_tab_is_and_the_cursor_keeps_its_character() {
    let mut setup = Setup::new("\tx\n");
    setup.set(|options| {
        options.number = false;
        options.relativenumber = false;
    });
    setup.keys("l");
    assert_eq!(setup.cursor(), (0, 4));
    setup.set(|options| options.tabstop = 8);
    assert_eq!(setup.cursor(), (0, 8));
    setup.editor.settle(&setup.copy);
    assert_eq!(setup.editor.scene().0[0].text(), "        x");
    setup.keys("x");
    assert_eq!(setup.text(), "\t\n");
}

#[test]
fn scrolloff_keeps_lines_in_view_around_the_cursor() {
    let mut setup = Setup::new(&"x\n".repeat(100));
    setup.set(|options| options.scrolloff = 2);
    // The pane is 10 lines tall.
    setup.keys("9j");
    assert_eq!(setup.view(), (2, 9, 0), "two lines below the cursor");
    setup.keys("6k");
    assert_eq!(setup.view(), (1, 3, 0), "and above it");
    setup.keys("G");
    assert_eq!(setup.view(), (90, 99, 0), "but not past the file's end");
    setup.keys("gg");
    assert_eq!(setup.view(), (0, 0, 0));
    // Scrolling moves the cursor, so the next key doesn't scroll back.
    setup.editor.wheel(-5, &setup.copy);
    assert_eq!(setup.view(), (5, 7, 0));
    setup.keys("<C-d>");
    assert_eq!(setup.view(), (10, 12, 0));
    // Half the pane or more keeps the cursor in the middle.
    setup.set(|options| options.scrolloff = 999);
    assert_eq!(setup.view(), (8, 12, 0));
    setup.keys("j");
    assert_eq!(setup.view(), (8, 13, 0));
    setup.keys("j");
    assert_eq!(setup.view(), (9, 14, 0));
    setup.set(|options| options.scrolloff = 0);
    setup.keys("9j");
    assert_eq!(setup.view(), (14, 23, 0), "none: the cursor at the bottom");
}

#[test]
fn the_view_never_jumps_on_the_key_after_a_scroll_or_a_click() {
    let mut setup = Setup::new(&"x\n".repeat(100));
    setup.set(|options| options.scrolloff = 2);
    // The wheel stops where the last line has scrolloff lines above it.
    setup.editor.wheel(-500, &setup.copy);
    let scrolled = setup.view();
    assert_eq!((scrolled.0, scrolled.1), (97, 99));
    setup.keys("l");
    assert_eq!(setup.view(), scrolled, "no jump back");
    // A click near the view's edge scrolls on the click.
    setup.keys("gg");
    setup.editor.wheel(-10, &setup.copy);
    setup.editor.click(1, 4, &setup.copy);
    let clicked = setup.view();
    assert_eq!(clicked.1, clicked.0 + 2, "scrolloff lines above");
    setup.keys("l");
    assert_eq!(setup.view().0, clicked.0);
}

#[test]
fn the_lines_around_decide_tabs_or_spaces_not_each_line() {
    // A file indented with tabs, though expandtab is on.
    let mut setup = Setup::new("func f() {\n\tx\n}\n");
    setup.keys(">2j");
    assert_eq!(setup.text(), "\tfunc f() {\n\t\tx\n\t}\n");
    let mut setup = Setup::new("\tfoo\n\tbar\n");
    setup.keys("<lt><lt>>>");
    assert_eq!(
        setup.text(),
        "\tfoo\n\tbar\n",
        "out and in again, a tab still"
    );
    setup.keys("jA<Tab><Esc>");
    assert_eq!(
        setup.text(),
        "\tfoo\n\tbar\t\n",
        "Tab, as the file has them"
    );
    // With nothing indented nearby, expandtab says.
    let mut setup = Setup::new("a\nb\n");
    setup.keys(">j");
    assert_eq!(setup.text(), "    a\n    b\n");
}

#[test]
fn a_huge_count_indents_only_so_far() {
    let mut setup = Setup::new("x\n");
    setup.keys("V99999999999999999999>");
    let indent = setup.text().len() - "x\n".len();
    assert_eq!(indent, 1000);
}

#[test]
fn cursorline_and_colorcolumn_mark_the_view() {
    let mut setup = Setup::new("one\ntwo\n");
    setup.keys("j");
    assert_eq!(
        setup.highlighted(HighlightKind::CursorLine),
        [(1, 4..40)],
        "right of the numbers, across the pane"
    );
    setup.set(|options| options.cursorline = false);
    assert_eq!(setup.highlighted(HighlightKind::CursorLine), []);
    // Column 3 is the text's third, right of the numbers. 100 isn't in
    // view, and nor are the rows past the end.
    setup.set(|options| options.colorcolumn = vec![3, 100]);
    assert_eq!(
        setup.highlighted(HighlightKind::ColorColumn),
        [(0, 6..7), (1, 6..7)]
    );

    // A column moves with the view, as it scrolls sideways.
    let mut setup = Setup::new(&"a".repeat(60));
    setup.set(|options| options.colorcolumn = vec![30]);
    setup.keys("$");
    assert_eq!(setup.editor.left, 24);
    assert_eq!(setup.highlighted(HighlightKind::ColorColumn), [(0, 9..10)]);
}

#[test]
fn search_matches_case_as_ignorecase_and_smartcase_say() {
    /// Where searching with `keys` from the line's start finds.
    fn found(setup: &mut Setup, keys: &str) -> usize {
        setup.keys(&format!("0{keys}<CR>"));
        setup.cursor().1
    }
    let mut setup = Setup::new("a two Two\n");
    // To begin with, case is ignored unless the search has a capital.
    assert_eq!(found(&mut setup, "/two"), 2);
    assert_eq!(found(&mut setup, "/Two"), 6);
    setup.set(|options| options.smartcase = false);
    assert_eq!(found(&mut setup, "/Two"), 2);
    setup.set(|options| options.ignorecase = false);
    assert_eq!(found(&mut setup, "/two"), 2);
    assert_eq!(found(&mut setup, "/TWO"), 0);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("TWO isn't in the file")
    );
}
