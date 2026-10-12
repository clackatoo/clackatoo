//! An editor driven by keys, with its buffer's copy talking to a daemon as
//! the real one does.

use clackatoo_protocol::{
    Answer, BufferId, BufferUpdate, ClientMessage, FileLocation, Query, Size,
};
use clackatoo_text::Sign;

use super::copy::tests::Daemon;
use super::keys::Stroke;
use super::{BufferCopy, Editor, HunkAction, Mode, Register};
use crate::options::Options;
use crate::pane::Selection;
use crate::render::{HighlightKind, Ink};

struct Setup {
    editor: Editor,
    copy: BufferCopy,
    daemon: Daemon,
    register: Register,
    options: Options,
    /// The queries sent, for the language server.
    queries: Vec<(u64, usize, Query)>,
    /// The renames sent, for the language server.
    renames: Vec<(u64, usize, String)>,
    /// The stages sent.
    stages: Vec<(u64, usize)>,
}

impl Setup {
    fn new(text: &str) -> Self {
        let daemon = Daemon::new(text);
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        // Without signs, which tests of them turn on, so the columns of
        // the rest are counted from the line numbers.
        let options = Options {
            gitsigns: false,
            ..Options::default()
        };
        let mut editor = Editor::new(BufferId(1), &options);
        editor.resize(Size::new(40, 10).unwrap());
        Self {
            editor,
            copy,
            daemon,
            register: Register::default(),
            options,
            queries: Vec::new(),
            renames: Vec::new(),
            stages: Vec::new(),
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

    /// Passes the window's changes to the daemon, and its answers back,
    /// keeping its queries for the language server.
    fn serve(&mut self) {
        loop {
            let messages = self.copy.take_outbox();
            if messages.is_empty() {
                return;
            }
            for message in messages {
                if let ClientMessage::Query { id, at, query, .. } = message {
                    self.queries.push((id, at, query));
                    continue;
                }
                if let ClientMessage::Rename { id, at, name, .. } = message {
                    self.renames.push((id, at, name));
                    continue;
                }
                if let ClientMessage::Stage { id, at, .. } = message {
                    self.stages.push((id, at));
                    continue;
                }
                let (answer, _) = self.daemon.take(message);
                self.editor.hold(&self.copy);
                let heard = self.copy.update(answer);
                self.editor.heard(&heard, true, &self.copy);
            }
        }
    }

    /// Turns the signs on, and gives the copy the hunks the daemon would
    /// find between `base` and the text now.
    fn hunks_against(&mut self, base: &str) {
        self.set(|options| options.gitsigns = true);
        let text = self.copy.text().to_string();
        let hunks = clackatoo_text::Hunks::between(base, &text);
        self.copy.update(BufferUpdate::Hunks {
            version: self.daemon.version(),
            hunks: hunks.all().to_vec(),
        });
    }

    /// Does `action` to the hunk at the cursor, as `<Space>h` and its
    /// kin ask.
    fn hunk(&mut self, action: HunkAction) {
        self.editor.hunk(action, &mut self.copy);
        self.serve();
    }

    /// Gives the editor the language server's answer to query `id`.
    fn answer(&mut self, id: u64, answer: Answer) {
        let heard = (self.copy).update(BufferUpdate::Answer {
            id,
            version: 0,
            answer,
        });
        self.editor.heard(&heard, true, &self.copy);
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
        gitsigns: false,
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

#[test]
fn diagnostics_are_underlined_told_of_in_the_bar_and_move_with_typing() {
    use clackatoo_protocol::{BufferUpdate, Diagnostic, Severity};
    use clackatoo_term::Underline;

    let mut setup = Setup::new("let x = y;\nok\n");
    let diagnostic = |start, end, severity, message: &str| Diagnostic {
        start,
        end,
        severity,
        message: message.to_owned(),
        source: Some("rustc".into()),
        code: Some("E0425".into()),
    };
    setup.copy.update(BufferUpdate::Diagnostics {
        version: 0,
        diagnostics: vec![
            diagnostic(8, 9, Severity::Error, "cannot find value `y`\nmore"),
            diagnostic(4, 5, Severity::Warning, "unused variable"),
        ],
    });
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    // After the four cells of line numbers.
    let cell = |row: &clackatoo_term::Row, col: usize| *row.style(row.cells()[4 + col]);
    assert_eq!(cell(rows[0], 8).underline, Underline::Curly);
    assert_eq!(
        cell(rows[0], 8).underline_color,
        crate::colors::diagnostic(Severity::Error)
    );
    assert_eq!(
        cell(rows[0], 4).underline_color,
        crate::colors::diagnostic(Severity::Warning)
    );
    assert_eq!(cell(rows[0], 0).underline, Underline::None);
    // The bar tells of the one that matters most on the cursor's line,
    // or else the one under the cursor.
    let label = setup.editor.label(&setup.copy);
    assert!(
        label.ends_with("error: cannot find value `y` [E0425 rustc]"),
        "{label}"
    );
    setup.keys("4l");
    let label = setup.editor.label(&setup.copy);
    assert!(
        label.ends_with("warning: unused variable [E0425 rustc]"),
        "{label}"
    );
    setup.keys("j");
    assert!(!setup.editor.label(&setup.copy).contains("error"));

    // Typing before them moves them along.
    setup.keys("ggImut <Esc>");
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    assert_eq!(cell(rows[0], 12).underline, Underline::Curly, "y, moved");
    assert_eq!(cell(rows[0], 8).underline, Underline::Curly, "x, moved");
    assert_eq!(cell(rows[0], 4).underline, Underline::None);
}

#[test]
fn diagnostics_are_written_after_lines_numbered_and_gone_to() {
    use clackatoo_protocol::{BufferUpdate, Diagnostic, Severity};

    // A pane wide enough for what's written after the lines.
    let mut setup = Setup::new("a\nlet x = y;\nb\nz;\n");
    setup.editor.resize(Size::new(60, 10).unwrap());
    let diagnostic = |start, severity, message: &str| Diagnostic {
        start,
        end: start + 1,
        severity,
        message: message.to_owned(),
        source: None,
        code: None,
    };
    setup.copy.update(BufferUpdate::Diagnostics {
        version: 0,
        diagnostics: vec![
            diagnostic(6, Severity::Warning, "unused"),
            diagnostic(10, Severity::Error, "cannot find `y`\nhelp: more"),
            diagnostic(15, Severity::Hint, "a hint"),
        ],
    });
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    // The worst of the line's, two cells after it, its first line only.
    assert_eq!(rows[1].text(), "  1 let x = y;  ■ cannot find `y`");
    assert_eq!(rows[3].text(), "  3 z;  ■ a hint");
    let fg = |row: &clackatoo_term::Row, col: usize| row.style(row.cells()[col]).fg;
    assert_eq!(fg(rows[1], 16), crate::colors::diagnostic(Severity::Error));
    // Its number in the same colour, but the cursor's line's.
    assert_eq!(fg(rows[1], 2), crate::colors::diagnostic(Severity::Error));
    assert_eq!(fg(rows[0], 2), crate::colors::line_number(true).fg);
    assert_eq!(fg(rows[2], 2), crate::colors::line_number(false).fg);

    // ]d and [d go from one to the next, round the file's ends.
    setup.keys("]d");
    assert_eq!(setup.cursor(), (1, 4));
    setup.keys("]d");
    assert_eq!(setup.cursor(), (1, 8));
    setup.keys("2]d");
    assert_eq!(setup.cursor(), (1, 4), "round past the end");
    setup.keys("[d");
    assert_eq!(setup.cursor(), (3, 0));

    // One past a line's end, as a missing semicolon is, is gone to and
    // past: the cursor lands on the line's last character. And a count
    // counts places, not diagnostics there together.
    setup.copy.update(BufferUpdate::Diagnostics {
        version: 0,
        diagnostics: vec![
            diagnostic(1, Severity::Error, "end of the first line"),
            diagnostic(6, Severity::Error, "x"),
            diagnostic(6, Severity::Warning, "also x"),
            diagnostic(15, Severity::Error, "z"),
        ],
    });
    setup.keys("gg]d");
    assert_eq!(setup.cursor(), (1, 4));
    setup.keys("]d]d");
    assert_eq!(setup.cursor(), (0, 0), "round to the first line's end");
    setup.keys("]d");
    assert_eq!(setup.cursor(), (1, 4), "and on past it");
    setup.keys("gg3]d");
    assert_eq!(
        setup.cursor(),
        (0, 0),
        "three places round, not three diagnostics"
    );

    // Off, nothing's written after the lines.
    setup.set(|options| options.virtualtext = false);
    setup.editor.settle(&setup.copy);
    assert_eq!(setup.editor.scene().0[1].text(), "  1 let x = y;");
}

#[test]
fn gd_asks_the_language_server_and_goes_where_it_says() {
    let mut setup = Setup::new("let x = y;\nfn y() {}\n");
    setup.keys("fygd");
    let (asked, at, query) = setup.queries[0];
    assert_eq!((at, query), (8, Query::Definition));
    let here = |line, col| FileLocation {
        path: "/a".into(),
        line,
        col,
        offset: None,
        text: String::new(),
    };
    setup.answer(asked, Answer::Locations(vec![here(1, 3)]));
    assert_eq!(setup.cursor(), (1, 3));
    assert_eq!(setup.editor.take_places(), None);

    // Several are for the desk to pick from, in the finder.
    setup.keys("gd");
    let (asked, ..) = setup.queries[1];
    let several = vec![here(0, 0), here(1, 0)];
    setup.answer(asked, Answer::Locations(several.clone()));
    assert_eq!(setup.editor.take_places(), Some(several));
    assert_eq!(setup.cursor(), (1, 3), "staying put");

    // Another file's is for the desk to open.
    setup.keys("gD");
    let (asked, ..) = setup.queries[2];
    let elsewhere = FileLocation {
        path: "/b.rs".into(),
        line: 9,
        col: 2,
        offset: None,
        text: String::new(),
    };
    setup.answer(asked, Answer::Locations(vec![elsewhere.clone()]));
    assert_eq!(setup.editor.take_opening(), Some(elsewhere));
    assert_eq!(setup.cursor(), (1, 3), "staying put here");

    // An answer to an older query, or another pane's, goes nowhere; and
    // what goes wrong is said.
    setup.keys("grr");
    assert_eq!(setup.queries[3].2, Query::References);
    setup.queries.remove(3);
    setup.keys("gri");
    let (asked, at, query) = setup.queries[3];
    assert_eq!((at, query), (14, Query::Implementation));
    setup.answer(asked - 1, Answer::Locations(vec![here(0, 0)]));
    assert_eq!(setup.cursor(), (1, 3));
    setup.answer(
        asked,
        Answer::Failed("its language server is still starting".into()),
    );
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("its language server is still starting")
    );
    // An empty list, which the daemon never sends, does nothing.
    setup.keys("gd");
    let (asked, ..) = setup.queries[4];
    setup.answer(asked, Answer::Locations(Vec::new()));
    assert_eq!(setup.cursor(), (1, 3));
}

/// The text of the lines the panel by the cursor shows, if it's shown.
fn panel_text(setup: &mut Setup) -> Option<Vec<String>> {
    setup.editor.settle(&setup.copy);
    let view = setup.editor.panel()?;
    Some(view.lines.iter().map(super::panel::Line::text).collect())
}

#[test]
fn k_shows_what_the_language_server_says_in_a_panel_by_the_cursor() {
    let mut setup = Setup::new("let x = y;\nfn y() {}\n");
    setup.keys("fyK");
    let (asked, at, query) = setup.queries[0];
    assert_eq!((at, query), (8, Query::Hover));
    let hover = |text: &str| Answer::Hover {
        text: text.to_owned(),
        markdown: true,
    };
    setup.answer(asked, hover("```rust\nfn y()\n```\n\nDoes **y**."));
    assert_eq!(panel_text(&mut setup).unwrap(), ["fn y()", "", "Does y."]);
    // Under the cursor's line, from its column.
    let placed = setup.editor.panel().unwrap().placed;
    assert_eq!((placed.top, placed.left), (1, 4 + 8));

    // K again, and Ctrl+E, scroll it rather than closing it; moving the
    // cursor closes it.
    setup.keys("K<C-e>");
    assert!(panel_text(&mut setup).is_some());
    setup.keys("l");
    assert_eq!(panel_text(&mut setup), None);

    // Esc closes it, as does an edit, though the cursor stays put.
    setup.keys("hK");
    setup.answer(setup.queries[1].0, hover("y"));
    setup.keys("<Esc>");
    assert_eq!(panel_text(&mut setup), None);
    setup.keys("$hK");
    setup.answer(setup.queries[2].0, hover("y"));
    assert!(panel_text(&mut setup).is_some());
    setup.keys("rz");
    assert_eq!(panel_text(&mut setup), None);
    // So does going somewhere else, as Ctrl+O does, and starting a search.
    setup.keys("K");
    setup.answer(setup.queries[3].0, hover("y"));
    setup.editor.go_to(0, 0, &setup.copy);
    assert_eq!(panel_text(&mut setup), None);
    setup.keys("$hK");
    setup.answer(setup.queries[4].0, hover("y"));
    setup.keys("/");
    assert_eq!(panel_text(&mut setup), None);
    setup.keys("<Esc>");
    // So does an undo, once the daemon has made it.
    setup.keys("K");
    setup.answer(setup.queries[5].0, hover("y"));
    assert!(panel_text(&mut setup).is_some());
    setup.keys("u");
    assert_eq!(panel_text(&mut setup), None);

    // An answer once the cursor has moved isn't shown; nothing to say is
    // said in the bar.
    setup.keys("K");
    let asked = setup.queries[6].0;
    setup.keys("0");
    setup.answer(asked, hover("y"));
    assert_eq!(panel_text(&mut setup), None);
    setup.keys("K");
    setup.answer(setup.queries[7].0, Answer::Nothing);
    assert_eq!(panel_text(&mut setup), None);
    assert_eq!(setup.editor.status.as_deref(), Some("no information"));

    // Esc closes it, and leaves visual mode, as it would have.
    setup.keys("vK");
    setup.answer(setup.queries.last().unwrap().0, hover("y"));
    assert!(panel_text(&mut setup).is_some());
    setup.keys("<Esc>");
    assert_eq!(panel_text(&mut setup), None);
    assert_eq!(setup.editor.mode, Mode::Normal);
}

#[test]
fn whats_wrong_is_shown_in_full_in_the_panel() {
    use clackatoo_protocol::{Diagnostic, Severity};

    let mut setup = Setup::new("let x = y;\nok\n");
    setup.copy.update(BufferUpdate::Diagnostics {
        version: 0,
        diagnostics: vec![Diagnostic {
            start: 8,
            end: 9,
            severity: Severity::Error,
            message: "cannot find value `y`\nin this scope".into(),
            source: Some("rustc".into()),
            code: Some("E0425".into()),
        }],
    });
    // Ctrl+W d on its line, though not on it, shows it.
    setup.keys("<C-w>d");
    assert_eq!(
        panel_text(&mut setup).unwrap(),
        [
            "error: cannot find value `y`",
            "in this scope",
            "[E0425 rustc]"
        ]
    );
    setup.keys("j<C-w>d");
    assert_eq!(panel_text(&mut setup), None);
    assert!(
        setup
            .editor
            .status
            .as_deref()
            .unwrap()
            .starts_with("nothing's wrong")
    );

    // K on it, where the server says nothing, shows it; and above what the
    // server says, when it says something.
    setup.keys("kfyK");
    setup.answer(setup.queries[0].0, Answer::Nothing);
    assert_eq!(panel_text(&mut setup).unwrap().len(), 3);
    setup.keys("<Esc>K");
    let hover = Answer::Hover {
        text: "y: u32".into(),
        markdown: false,
    };
    setup.answer(setup.queries[1].0, hover);
    let text = panel_text(&mut setup).unwrap();
    assert_eq!(text.last().map(String::as_str), Some("y: u32"));
    assert!(text[3].starts_with('─'), "{text:?}");
}

#[test]
fn grn_asks_in_the_bar_for_a_new_name_and_renames() {
    let mut setup = Setup::new("let value = 1;\nvalue + 1\n");
    let mut copy = BufferCopy::new(BufferId(1));
    copy.update(setup.daemon.snapshot());
    setup.keys("jgrn");
    // Started with the word under the cursor.
    assert_eq!(setup.editor.label(&copy), "rename to: value▏");
    setup.keys("<C-u>total<CR>");
    let (asked, at, name) = setup.renames[0].clone();
    assert_eq!((at, name.as_str()), ("let value = 1;\n".len(), "total"));
    // K while the rename is on its way doesn't lose its answer.
    setup.keys("K");
    setup.answer(setup.queries[0].0, Answer::Nothing);
    setup.answer(
        asked,
        Answer::Renamed {
            occurrences: 2,
            files: 1,
        },
    );
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("renamed 2 occurrences in 1 file")
    );

    // Esc, or the name unchanged, renames nothing; what goes wrong is
    // said.
    setup.keys("grn<Esc>grn<CR>");
    assert_eq!(setup.editor.status.as_deref(), Some("not renamed"));
    assert_eq!(setup.renames.len(), 1);
    setup.keys("grnX<BS>s<CR>");
    let (asked, _, name) = setup.renames[1].clone();
    assert_eq!(name, "values");
    setup.answer(
        asked,
        Answer::Failed("its language server can't rename".into()),
    );
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("its language server can't rename")
    );
    // Typed text goes to the name, not to the file.
    assert_eq!(setup.text(), "let value = 1;\nvalue + 1\n");
}

#[test]
fn percent_goes_to_the_bracket_that_pairs() {
    let mut setup = Setup::new("fn f(a, (b)) {\n  x()\n}\n");
    // From before any, to the pair of the first on the line.
    setup.keys("%");
    assert_eq!(setup.cursor(), (0, 11));
    setup.keys("%");
    assert_eq!(setup.cursor(), (0, 4));
    // Across lines, past brackets of other kinds.
    setup.keys("$%");
    assert_eq!(setup.cursor(), (2, 0));
    setup.keys("%");
    assert_eq!(setup.cursor(), (0, 13));
    // With an operator, both brackets go.
    setup.keys("0f(d%");
    assert_eq!(setup.text(), "fn f {\n  x()\n}\n");
}

#[test]
fn braces_go_to_the_blank_lines_round_paragraphs() {
    let mut setup = Setup::new("a\nb\n\nc\nd\n\ne\n");
    setup.keys("}");
    assert_eq!(setup.cursor(), (2, 0));
    setup.keys("}");
    assert_eq!(setup.cursor(), (5, 0));
    setup.keys("{");
    assert_eq!(setup.cursor(), (2, 0));
    setup.keys("2{");
    assert_eq!(
        setup.cursor(),
        (0, 0),
        "the first line, with no blank before"
    );
    // A jump, which Ctrl+O comes back from, is the desk's: here, `}`
    // with an operator deletes to the blank line.
    setup.keys("d}");
    assert_eq!(setup.text(), "\nc\nd\n\ne\n");
}

#[test]
fn star_and_hash_search_for_the_word_under_the_cursor_whole() {
    let mut setup = Setup::new("foo foobar foo\nfoo\n");
    setup.keys("*");
    assert_eq!(setup.cursor(), (0, 11), "not foobar");
    setup.keys("n");
    assert_eq!(setup.cursor(), (1, 0));
    setup.keys("#");
    assert_eq!(setup.cursor(), (0, 11));
    setup.keys("N");
    assert_eq!(setup.cursor(), (1, 0), "N goes the other way from #");
    // Over no word, the next on the line, as Vim's.
    let mut blank = Setup::new("a  b b\n");
    blank.keys("l*");
    assert_eq!(blank.cursor(), (0, 5));
    // With none after it, it says so.
    let mut none = Setup::new("a  \n");
    none.keys("$*");
    assert_eq!(none.cursor(), (0, 0));
}

#[test]
fn motions_that_go_nowhere_take_nothing_and_the_last_paragraph_goes_to_the_end() {
    // `%` with no bracket on the line, and `f` with no such character,
    // leave the text alone, as Vim's do.
    let mut setup = Setup::new("hello\nworld\n");
    setup.keys("d%");
    setup.keys("dfz");
    assert_eq!(setup.text(), "hello\nworld\n");
    // `}` with no empty line after goes to the end of the last line, and
    // `d}` takes it all.
    setup.keys("}");
    assert_eq!(setup.cursor(), (1, 4));
    setup.keys("ggd}");
    assert_eq!(setup.text(), "\n");
    // A line of blanks isn't empty: the paragraph goes on past it.
    let mut blanks = Setup::new("a\n  \nb\n\nc\n");
    blanks.keys("}");
    assert_eq!(blanks.cursor(), (3, 0));
}

#[test]
fn star_ignores_case_as_ignorecase_says_whatever_smartcase_says() {
    // Both on, as by default: `*` on "Foo" finds "foo".
    let mut setup = Setup::new("Foo bar foo\n");
    setup.keys("*");
    assert_eq!(setup.cursor(), (0, 8));
}

#[test]
fn signs_are_drawn_left_of_the_numbers_and_can_be_turned_off() {
    let mut setup = Setup::new("a\nB\nc\nnew\nd\n");
    // B changed, a line added, and e deleted after d.
    setup.hunks_against("a\nb\nc\nd\ne\n");
    setup.editor.settle(&setup.copy);
    let (rows, cursor) = setup.editor.scene();
    let signs: Vec<char> = rows[..5]
        .iter()
        .map(|row| row.text().chars().next().unwrap())
        .collect();
    assert_eq!(signs, [' ', '▎', ' ', '▎', '▁']);
    assert_eq!(rows[1].text(), "▎  1 B");
    let fg = |row: &clackatoo_term::Row| row.style(row.cells()[0]).fg;
    assert_eq!(fg(rows[1]), crate::colors::sign(Sign::Changed).1.fg);
    assert_eq!(fg(rows[3]), crate::colors::sign(Sign::Added).1.fg);
    assert_eq!(fg(rows[4]), crate::colors::sign(Sign::Deleted).1.fg);
    assert_eq!(cursor.unwrap().col, 5, "right of the signs and numbers");
    // A click lands on the text beside them.
    setup.editor.click(1, 5, &setup.copy);
    assert_eq!(setup.cursor(), (1, 0));

    // They move with typing until the daemon compares again.
    setup.keys("ggOtop<Esc>");
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    assert!(rows[2].text().starts_with('▎'), "{:?}", rows[2].text());

    setup.set(|options| options.gitsigns = false);
    setup.editor.settle(&setup.copy);
    let (rows, cursor) = setup.editor.scene();
    assert_eq!(rows[2].text(), "  2 B");
    assert_eq!(cursor.unwrap().col, 4 + 2, "on the p of top");
}

#[test]
fn brackets_and_c_go_round_the_hunks_as_jumps() {
    let base = "0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    let mut setup = Setup::new("0\n1\nTWO\n3\n4\nFIVE\n6\n7\n9\n");
    // Changed at 2 and 5, and 8 deleted after 7.
    setup.hunks_against(base);
    setup.keys("]c");
    assert_eq!(setup.cursor(), (2, 0));
    assert_eq!(setup.editor.take_left(), Some(0), "a jump");
    setup.keys("]c");
    assert_eq!(setup.cursor(), (5, 0));
    setup.keys("]c");
    assert_eq!(setup.cursor(), (7, 0), "on the line above lines deleted");
    setup.keys("]c");
    assert_eq!(setup.cursor(), (2, 0), "round from the last");
    setup.keys("[c");
    assert_eq!(setup.cursor(), (7, 0), "round from the first");
    setup.keys("2[c");
    assert_eq!(setup.cursor(), (2, 0));
    // From the window's keys and commands too.
    setup.hunk(HunkAction::Next { forward: true });
    assert_eq!(setup.cursor(), (5, 0));

    let mut setup = Setup::new("same\n");
    setup.hunks_against("same\n");
    setup.keys("]c");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("nothing's changed since git's index")
    );
}

#[test]
fn a_hunk_is_previewed_by_the_cursor() {
    let mut setup = Setup::new("a\nB\nb2\nc\n");
    setup.hunks_against("a\nb\nc\n");
    setup.keys("j");
    setup.hunk(HunkAction::Preview);
    setup.editor.settle(&setup.copy);
    let view = setup.editor.panel().expect("a panel");
    let lines: Vec<String> = view.lines.iter().map(super::panel::Line::text).collect();
    assert_eq!(lines, ["-b", "+B", "+b2"]);
    let inks: Vec<_> = view.lines.iter().map(|line| line.runs[0].ink).collect();
    assert_eq!(inks, [Ink::Removed, Ink::Added, Ink::Added]);
    // Moving away closes it; a line not changed has nothing to show.
    setup.keys("k");
    assert!(setup.editor.panel.is_none());
    setup.hunk(HunkAction::Preview);
    assert!(setup.editor.panel.is_none());
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("nothing's changed here since git's index")
    );
}

#[test]
fn a_hunk_is_reset_as_an_edit_u_undoes() {
    let mut setup = Setup::new("a\nB\nc\nnew\n");
    setup.hunks_against("a\nb\nc\n");
    setup.keys("j");
    setup.hunk(HunkAction::Reset);
    assert_eq!(setup.text(), "a\nb\nc\nnew\n");
    assert_eq!(setup.cursor(), (1, 0));
    // Only the added line is left, where it was: the hunk put back went
    // with it rather than staying as lines deleted.
    let left: Vec<(usize, usize)> = (setup.copy.hunks().all().iter())
        .map(|hunk| (hunk.start, hunk.end))
        .collect();
    assert_eq!(left, [(6, 10)]);
    setup.keys("u");
    assert_eq!(setup.text(), "a\nB\nc\nnew\n");
    setup.keys("<C-r>");

    // Lines added go; lines deleted come back, from the line above them.
    setup.keys("G");
    setup.hunk(HunkAction::Reset);
    assert_eq!(setup.text(), "a\nb\nc\n");
    let mut setup = Setup::new("a\nc\n");
    setup.hunks_against("a\nb\nc\n");
    setup.hunk(HunkAction::Reset);
    assert_eq!(setup.text(), "a\nb\nc\n");
    assert!(setup.copy.hunks().is_empty());
    setup.keys("u");
    assert_eq!(setup.text(), "a\nc\n");
    // From visual mode, which it ends, as an operator does.
    setup.hunks_against("a\nb\nc\n");
    setup.keys("jVgg");
    setup.hunk(HunkAction::Reset);
    assert_eq!(setup.text(), "a\nb\nc\n");
    assert_eq!(setup.editor.mode, Mode::Normal);
}

#[test]
fn a_hunk_moved_into_a_line_is_reset_and_shown_whole() {
    let mut setup = Setup::new("a\nB\nc\n");
    setup.hunks_against("a\nb\nc\n");
    // Joined to the line above before the daemon compares again: the
    // hunk now starts in the middle of a line.
    setup.keys("J");
    assert_eq!(setup.text(), "a B\nc\n");
    assert_eq!(setup.copy.hunks().all()[0].start, 2);
    setup.hunk(HunkAction::Preview);
    setup.editor.settle(&setup.copy);
    let view = setup.editor.panel().expect("a panel");
    let lines: Vec<String> = view.lines.iter().map(super::panel::Line::text).collect();
    assert_eq!(lines, ["-b", "+a B"]);
    setup.hunk(HunkAction::Reset);
    assert_eq!(setup.text(), "b\nc\n", "whole lines, not from mid-line");
}

#[test]
fn a_hunk_is_staged_by_the_daemon_and_the_bar_says_so() {
    let mut setup = Setup::new("a\nB\n");
    setup.hunks_against("a\nb\n");
    // Nothing to stage on a line that hasn't changed.
    setup.hunk(HunkAction::Stage);
    assert!(setup.stages.is_empty());
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("nothing's changed here since git's index")
    );
    setup.keys("j");
    setup.hunk(HunkAction::Stage);
    let [(id, at)] = setup.stages[..] else {
        panic!("one stage: {:?}", setup.stages);
    };
    assert_eq!(at, 2, "the cursor's place, on the hunk's line");
    setup.answer(id, Answer::Staged);
    assert_eq!(setup.editor.status.as_deref(), Some("hunk staged"));
    setup.hunk(HunkAction::Stage);
    let (id, _) = setup.stages[1];
    setup.answer(id, Answer::Failed("git couldn't stage it: no".into()));
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("git couldn't stage it: no")
    );
    // Two asked before either is answered: each answer is heard.
    setup.hunk(HunkAction::Stage);
    setup.hunk(HunkAction::Stage);
    let (first, second) = (setup.stages[2].0, setup.stages[3].0);
    setup.answer(first, Answer::Failed("first".into()));
    assert_eq!(setup.editor.status.as_deref(), Some("first"));
    setup.answer(second, Answer::Staged);
    assert_eq!(setup.editor.status.as_deref(), Some("hunk staged"));
}
