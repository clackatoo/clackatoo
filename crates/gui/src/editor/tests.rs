//! An editor driven by keys, with its buffer's copy talking to a daemon as
//! the real one does.

use std::time::{Duration, Instant};

use clackatoo_protocol::{
    Answer, BufferId, BufferUpdate, ClientMessage, CodeAction, CompletionItem, Construct,
    FileLocation, Piece, Query, ServerFeatures, Size, TextChange, Trigger,
};
use clackatoo_text::Sign;

use super::copy::tests::Daemon;
use super::{BufferCopy, Editor, HunkAction, Mode, Register, Shared, Stroke, Wrote};
use crate::motion::{Cursor, Point};
use crate::options::{CompleteAccept, Options};
use crate::render::{HighlightKind, Ink};
use crate::vim::Selection;

mod block;
mod hints;
mod indent;
mod motions;
mod mouse;
mod prose;
mod rendered;
mod snippets;
mod substitute;
mod uses;
mod visual;
mod words;
mod wrapped;

struct Setup {
    editor: Editor,
    copy: BufferCopy,
    daemon: Daemon,
    shared: Shared,
    options: Options,
    /// The queries sent, for the language server.
    queries: Vec<(u64, usize, Query)>,
    /// The questions how lines should be indented, for the grammar, kept
    /// apart.
    indents: Vec<(u64, usize, Query)>,
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
            shared: Shared::default(),
            options,
            queries: Vec::new(),
            indents: Vec::new(),
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
        (self.editor.top.line, line, col)
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
        for stroke in Stroke::read(keys) {
            self.editor
                .typed(stroke, &mut self.copy, &mut self.shared, false);
            self.follow_marks();
            self.serve();
        }
    }

    /// Moves the marks through the edits just made, as the desk does.
    fn follow_marks(&mut self) {
        let made = self.copy.take_made();
        (self.shared.marks).edited(self.copy.file(), &made, self.copy.text());
    }

    /// Moves the marks through an edit the copy heard of, an undo or
    /// another's, as the desk does.
    fn heard_marks(&mut self, heard: &super::Heard) {
        let (file, text) = (self.copy.file(), self.copy.text());
        match heard {
            super::Heard::Edited(edit) | super::Heard::Reversed(edit) => {
                (self.shared.marks).edited(file, std::slice::from_ref(edit), text);
            }
            super::Heard::Replaced => self.shared.marks.replaced(file, text),
            _ => {}
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
                    if let Query::Indent { .. } = query {
                        self.indents.push((id, at, query));
                    } else {
                        self.queries.push((id, at, query));
                    }
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
                let mut heard = self.copy.update(answer);
                self.heard_marks(&heard);
                self.editor.heard(&mut heard, true, &mut self.copy);
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

    /// Gives the editor the language server's answer to query `id`, about
    /// the daemon's text as it is now.
    fn answer(&mut self, id: u64, answer: Answer) {
        let mut heard = (self.copy).update(BufferUpdate::Answer {
            id,
            version: self.daemon.version(),
            answer,
        });
        self.editor.heard(&mut heard, true, &mut self.copy);
        // As the desk does, for a command that waited for it, and a new
        // line's indent.
        if self.editor.resumable() {
            self.editor.resume(&mut self.copy, &mut self.shared);
        }
        if self.editor.correcting() {
            self.editor
                .correct_indents(&mut self.copy, &mut self.shared);
        }
        self.serve();
    }

    fn text(&self) -> String {
        let text = self.copy.text().to_string();
        assert_eq!(
            text,
            self.daemon.text().to_string(),
            "the daemon has the same"
        );
        text
    }

    /// What `p` puts.
    fn register(&self) -> Register {
        (self.shared.registers.get(None).cloned()).unwrap_or_default()
    }

    /// What the register `name` holds.
    fn held(&self, name: char) -> Option<String> {
        let register = self.shared.registers.get(Some(name))?;
        Some(register.text.clone())
    }

    fn cursor(&self) -> (u64, usize) {
        (self.editor.cursor.at.line, self.editor.cursor.at.col)
    }
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
    assert_eq!(setup.register().text, "abc\ndef\n");
    assert!(setup.register().lines);
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
fn a_search_that_goes_round_an_end_says_so() {
    let mut setup = Setup::new("a two\nb two\nc\n");
    setup.keys("/two<CR>");
    assert_eq!(setup.editor.status, None);
    setup.keys("n");
    assert_eq!(setup.editor.status, None);
    setup.keys("n");
    assert_eq!(setup.cursor(), (0, 2));
    assert_eq!(
        setup.editor.status.as_deref(),
        Some(crate::pane::WRAPPED_TO_TOP)
    );
    setup.keys("n");
    assert_eq!(setup.editor.status, None, "it says it once");
    setup.keys("N");
    assert_eq!(setup.editor.status, None);
    setup.keys("N");
    assert_eq!(setup.cursor(), (1, 2));
    assert_eq!(
        setup.editor.status.as_deref(),
        Some(crate::pane::WRAPPED_TO_BOTTOM)
    );
    // The only match, from on it, goes all the way round to itself.
    let mut setup = Setup::new("one\nx\n");
    setup.keys("/one<CR>");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some(crate::pane::WRAPPED_TO_TOP)
    );
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
    let mut heard = setup.copy.update(update);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
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
fn the_lines_past_the_end_are_marked_faintly() {
    let mut setup = Setup::new("a\n");
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    let past = &rows[1..];
    assert!(!past.is_empty());
    for row in past {
        assert_eq!(row.text(), "~");
        let style = row.style(row.cells()[0]);
        assert_eq!(*style, crate::colors::end_of_buffer());
        assert_ne!(style.fg, crate::colors::line_number(false).fg);
    }
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
    setup.editor.click(1, 6, &mut setup.copy);
    assert_eq!(setup.cursor(), (1, 2));
    setup.editor.click(0, 1, &mut setup.copy);
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
    let kept = Register::chars("kept".into());
    setup.shared.registers.wrote(None, kept, Wrote::Yank);
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
    assert_eq!(setup.register().text, "kept", "the register isn't touched");
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
    setup.editor.click(1, 4, &mut setup.copy);
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
    setup.set(|options| {
        options.colorcolumn = vec![30];
        options.wrap = false;
    });
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
    // In each severity's colour, toned down: as it's drawn, readable at a
    // glance but quieter than the code, and not the number's full red.
    let theme = crate::colors::Theme::default();
    let drawn = |row: &clackatoo_term::Row, col: usize| {
        let said = theme.cell(row.style(row.cells()[col])).foreground;
        crate::colors::contrast(said, theme.background)
    };
    let code = crate::colors::contrast(theme.foreground, theme.background);
    for (row, col, severity) in [(rows[1], 16, Severity::Error), (rows[3], 8, Severity::Hint)] {
        assert_eq!(fg(row, col), crate::colors::virtual_text(severity).fg);
        assert!(fg(row, col) != crate::colors::diagnostic(severity));
        let contrast = drawn(row, col);
        assert!(
            contrast >= 4.5 && contrast < code / 2.0,
            "{severity:?}: {contrast}"
        );
    }
    assert_ne!(fg(rows[1], 16), fg(rows[3], 8), "a hint isn't an error");
    // Its number in the full colour, but the cursor's line's.
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
        range: None,
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
fn k_underlines_what_its_about_and_colours_its_code_once_told() {
    use clackatoo_term::Underline;
    use clackatoo_text::{Kind, Span};

    let mut setup = Setup::new("let x = y;\nfn y() {}\n");
    setup.keys("fyK");
    let asked = setup.queries[0].0;
    let text = "```rust\nfn y()\n```\n\nDoes **y**.";
    setup.answer(
        asked,
        Answer::Hover {
            text: text.into(),
            markdown: true,
            range: Some((8, 9)),
        },
    );
    // After the four cells of line numbers.
    let underline = |setup: &mut Setup, col: usize| {
        setup.editor.settle(&setup.copy);
        let (rows, _) = setup.editor.scene();
        let row = rows[0];
        row.style(row.cells()[4 + col]).underline
    };
    assert_eq!(underline(&mut setup, 8), Underline::Single);
    assert_eq!(underline(&mut setup, 7), Underline::None);
    assert_eq!(underline(&mut setup, 9), Underline::None);
    // Its code is plain until its colours come, then coloured, for this
    // query only.
    let code_runs = |setup: &mut Setup| {
        setup.editor.settle(&setup.copy);
        let view = setup.editor.panel().unwrap();
        (view.lines[0].runs.iter())
            .map(|run| (run.text.clone(), run.ink))
            .collect::<Vec<_>>()
    };
    assert_eq!(code_runs(&mut setup), [("fn y()".to_owned(), Ink::Plain)]);
    let colour = |setup: &mut Setup, id| {
        let at = text.find("fn y").unwrap();
        let spans = vec![Span {
            start: at,
            end: at + 2,
            kind: Kind::Keyword,
        }];
        let mut heard = setup.copy.update(BufferUpdate::Coloured { id, spans });
        setup.editor.heard(&mut heard, true, &mut setup.copy);
    };
    colour(&mut setup, asked + 1);
    assert_eq!(code_runs(&mut setup), [("fn y()".to_owned(), Ink::Plain)]);
    colour(&mut setup, asked);
    let keyword = Ink::Color(crate::colors::syntax(Kind::Keyword).fg);
    assert_eq!(
        code_runs(&mut setup),
        [("fn".to_owned(), keyword), (" y()".to_owned(), Ink::Plain)]
    );

    // An edit from elsewhere before it moves it along...
    let agents = setup.daemon.agent(0, "  ");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(agents);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    assert!(setup.editor.panel().is_some());
    assert_eq!(underline(&mut setup, 10), Underline::Single);
    assert_eq!(underline(&mut setup, 8), Underline::None);
    // ...and one beside it changes it: it's no longer underlined, though
    // the panel stays.
    let agents = setup.daemon.agent(11, "z");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(agents);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    assert!(setup.editor.panel().is_some());
    assert_eq!(underline(&mut setup, 10), Underline::None);

    // Closing the panel, as moving the cursor does, takes it away.
    setup.keys("<Esc>K");
    setup.answer(
        setup.queries[1].0,
        Answer::Hover {
            text: "y".into(),
            markdown: false,
            range: Some((10, 11)),
        },
    );
    assert_eq!(underline(&mut setup, 10), Underline::Single);
    setup.keys("l");
    assert_eq!(panel_text(&mut setup), None);
    assert_eq!(underline(&mut setup, 10), Underline::None);
}

#[test]
fn leaving_an_editor_stops_waiting_for_what_k_asked() {
    let mut setup = Setup::new("let x = y;\n");
    setup.keys("fyK");
    let asked = setup.queries[0].0;
    assert!(setup.editor.waiting_for_answer(&setup.copy, asked));
    // Another pane's K may have it cancelled: it's no longer waited for,
    // nor shown if it comes.
    setup.editor.leave();
    assert!(!setup.editor.waiting_for_answer(&setup.copy, asked));
    let hover = Answer::Hover {
        text: "y".into(),
        markdown: false,
        range: None,
    };
    setup.answer(asked, hover);
    assert_eq!(panel_text(&mut setup), None);
    // A jump asked for is still waited for.
    setup.keys("gd");
    let asked = setup.queries[1].0;
    setup.editor.leave();
    assert!(setup.editor.waiting_for_answer(&setup.copy, asked));
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
        range: None,
    };
    setup.answer(setup.queries[1].0, hover);
    let text = panel_text(&mut setup).unwrap();
    assert_eq!(text.last().map(String::as_str), Some("y: u32"));
    assert!(text[3].is_empty(), "{text:?}");
    assert!(setup.editor.panel().unwrap().lines[3].rule, "a line across");
}

#[test]
fn grn_asks_in_the_bar_for_a_new_name_and_renames() {
    let mut setup = Setup::new("let value = 1;\nvalue + 1\n");
    let mut copy = BufferCopy::new(BufferId(1));
    copy.update(setup.daemon.snapshot());
    setup.keys("jgrn");
    // Started with the word under the cursor, selected, so the first key
    // typed replaces it.
    assert_eq!(setup.editor.label(&copy), "rename to: value▏");
    let selected = setup.editor.label_selected().unwrap();
    assert_eq!(&setup.editor.label(&copy)[selected], "value");
    setup.keys("total");
    assert_eq!(setup.editor.label(&copy), "rename to: total▏");
    assert_eq!(setup.editor.label_selected(), None);
    setup.keys("<CR>");
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
    // An arrow keeps the old name, to edit.
    setup.keys("grn<End>X<BS>s<CR>");
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
fn grn_asks_a_server_that_can_tell_first_and_starts_the_name_as_it_says() {
    let mut setup = Setup::new("let value = 1;\nvalue + 1\n");
    setup.copy.update(BufferUpdate::Features(ServerFeatures {
        prepare_rename: true,
        ..ServerFeatures::default()
    }));
    let label = |setup: &Setup| setup.editor.label(&setup.copy);
    // Asked first, at the cursor, before anything's asked in the bar.
    setup.keys("jgrn");
    let (asked, at, query) = setup.queries[0];
    assert_eq!(
        (at, query),
        ("let value = 1;\n".len(), Query::PrepareRename)
    );
    assert!(!label(&setup).starts_with("rename to"), "not yet");
    // Started as the server says...
    let renamable = |name: Option<&str>| Answer::Renamable {
        name: name.map(str::to_owned),
    };
    setup.answer(asked, renamable(Some("the_value")));
    assert_eq!(label(&setup), "rename to: the_value▏");
    setup.keys("<CR>");
    assert_eq!(setup.editor.status.as_deref(), Some("not renamed"));
    // ...or, if it leaves that to the window, as the word there.
    setup.keys("grn");
    setup.answer(setup.queries[1].0, renamable(None));
    assert_eq!(label(&setup), "rename to: value▏");
    setup.keys("<Esc>");

    // Nothing there, or what's wrong, is said at once, asking nothing.
    setup.keys("grn");
    setup.answer(setup.queries[2].0, Answer::Nothing);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("there's nothing here to rename")
    );
    setup.keys("grn");
    let why = "that can't be renamed";
    setup.answer(setup.queries[3].0, Answer::Failed(why.into()));
    assert_eq!(setup.editor.status.as_deref(), Some(why));
    assert!(!label(&setup).starts_with("rename to"));
    // An answer for where the cursor was asks nothing either.
    setup.keys("grnl");
    setup.answer(setup.queries[4].0, renamable(Some("x")));
    assert!(!label(&setup).starts_with("rename to"));
    // Nor one about a text changed meanwhile, the cursor where it was.
    setup.keys("hgrn");
    let end = setup.copy.text().len_bytes();
    let update = setup.daemon.agent(end, "// more\n");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(update);
    setup.editor.heard(&mut heard, true, &mut setup.copy);
    setup.answer(setup.queries[5].0, renamable(Some("x")));
    assert!(!label(&setup).starts_with("rename to"));
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("the text changed meanwhile: not renamed")
    );
    assert!(setup.renames.is_empty());
    assert_eq!(setup.text(), "let value = 1;\nvalue + 1\n// more\n");
}

fn code_action(title: &str, kind: &str, disabled: Option<&str>) -> CodeAction {
    CodeAction {
        title: title.into(),
        kind: kind.into(),
        preferred: false,
        disabled: disabled.map(str::to_owned),
    }
}

/// The lines of the menu of code actions, if it's shown.
fn action_menu(setup: &mut Setup) -> Option<Vec<(String, bool)>> {
    setup.editor.settle(&setup.copy);
    let menu = setup.editor.actions.as_ref()?.view()?;
    Some(
        (menu.lines.iter())
            .map(|line| (line.text(), line.picked))
            .collect(),
    )
}

#[test]
fn gra_asks_for_code_actions_at_the_cursor_and_picks_one_from_a_menu() {
    let mut setup = Setup::new("let value = 1;\nvalue + 1\n");
    let line = "let value = 1;\n".len();
    setup.keys("jgra");
    // Asked of the cursor alone.
    let (list, at, query) = setup.queries[0];
    assert_eq!((at, query), (line, Query::CodeActions { end: line }));
    let actions = vec![
        code_action("Fix it", "quickfix", None),
        code_action("Rewrite it", "refactor.rewrite", None),
        code_action("Can't", "refactor", Some("not here")),
    ];
    setup.answer(list, Answer::CodeActions(actions.clone()));
    assert_eq!(
        action_menu(&mut setup).unwrap(),
        [
            ("1  Fix it  quickfix".to_owned(), true),
            ("2  Rewrite it  refactor.rewrite".to_owned(), false),
            ("3  Can't  not here".to_owned(), false),
        ]
    );
    // j moves through the menu, not the cursor, and Enter picks.
    setup.keys("j");
    assert_eq!(setup.cursor(), (1, 0));
    assert!(action_menu(&mut setup).unwrap()[1].1);
    setup.keys("<CR>");
    assert_eq!(setup.queries[1].2, Query::Act { list, item: 1 });
    assert!(action_menu(&mut setup).is_none(), "picking closes it");
    // K while it's done doesn't lose its answer.
    setup.keys("K");
    setup.answer(setup.queries[2].0, Answer::Nothing);
    let acted = Answer::Acted {
        title: "Rewrite it".into(),
        files: 1,
    };
    setup.answer(setup.queries[1].0, acted);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("Rewrite it: changed 1 file")
    );

    // A number picks at once; one that can't be done now isn't asked for.
    setup.keys("gra");
    setup.answer(setup.queries[3].0, Answer::CodeActions(actions.clone()));
    setup.keys("3");
    assert_eq!(setup.editor.status.as_deref(), Some("Can't: not here"));
    assert_eq!(setup.queries.len(), 4);
    setup.keys("gra");
    let list = setup.queries[4].0;
    setup.answer(list, Answer::CodeActions(actions.clone()));
    setup.keys("1");
    assert_eq!(setup.queries[5].2, Query::Act { list, item: 0 });
    setup.answer(
        setup.queries[5].0,
        Answer::Failed("it changed meanwhile".into()),
    );
    assert_eq!(setup.editor.status.as_deref(), Some("it changed meanwhile"));

    // Esc closes it; another key closes it and does what it does.
    setup.keys("gra");
    setup.answer(setup.queries[6].0, Answer::CodeActions(actions.clone()));
    setup.keys("<Esc>");
    assert!(action_menu(&mut setup).is_none());
    setup.keys("gra");
    setup.answer(setup.queries[7].0, Answer::CodeActions(actions.clone()));
    setup.keys("l");
    assert!(action_menu(&mut setup).is_none());
    assert_eq!(setup.cursor(), (1, 1));
    // Answered once the cursor has moved on, it's not shown.
    setup.keys("grah");
    setup.answer(setup.queries[8].0, Answer::CodeActions(actions));
    assert!(action_menu(&mut setup).is_none());
    // None, or none to be had, is said.
    setup.keys("gra");
    setup.answer(setup.queries[9].0, Answer::CodeActions(Vec::new()));
    assert_eq!(setup.editor.status.as_deref(), Some("no code actions here"));
    setup.keys("gra");
    let failed = Answer::Failed("its language server has no code actions".into());
    setup.answer(setup.queries[10].0, failed);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("its language server has no code actions")
    );
    assert_eq!(setup.text(), "let value = 1;\nvalue + 1\n");
}

#[test]
fn the_menu_of_code_actions_is_normal_modes_alone() {
    let mut setup = Setup::new("let value = 1;\n");
    let actions = vec![code_action("Fix it", "quickfix", None)];
    // Answered once insert mode has begun, it isn't shown, and j, Enter
    // and a digit are typed.
    setup.keys("gra");
    setup.keys("i");
    setup.answer(setup.queries[0].0, Answer::CodeActions(actions.clone()));
    assert!(action_menu(&mut setup).is_none());
    setup.keys("j1<Esc>");
    assert_eq!(setup.text(), "j1let value = 1;\n");
    assert_eq!(setup.queries.len(), 1, "nothing picked");
    // Nor once a search is being typed.
    setup.keys("gra/");
    setup.answer(setup.queries[1].0, Answer::CodeActions(actions.clone()));
    assert!(action_menu(&mut setup).is_none());
    setup.keys("<Esc>");
    // Shown, then insert mode: it closes, and keys are typed.
    setup.keys("gra");
    setup.answer(setup.queries[2].0, Answer::CodeActions(actions));
    assert!(action_menu(&mut setup).is_some());
    setup.keys("ak<Esc>");
    assert!(action_menu(&mut setup).is_none());
    assert_eq!(setup.text(), "j1klet value = 1;\n");
    assert_eq!(setup.queries.len(), 3);
}

#[test]
fn gra_in_visual_mode_asks_for_code_actions_on_the_selection() {
    let mut setup = Setup::new("let value = 1;\nvalue + 1\n");
    let line = "let value = 1;\n".len();
    // From the second line's start to the end of its first word.
    setup.keys("jvegra");
    assert_eq!(setup.editor.mode, Mode::Normal, "visual mode is left");
    let (_, at, query) = setup.queries[0];
    assert_eq!(
        (at, query),
        (
            line,
            Query::CodeActions {
                end: line + "value".len()
            }
        )
    );
    // Selected backwards, the same.
    setup.keys("vbgra");
    assert_eq!(setup.queries[1].1, line);
    assert_eq!(
        setup.queries[1].2,
        Query::CodeActions {
            end: line + "value".len()
        }
    );
    // Whole lines, from the first's start to the last's end.
    setup.keys("kVjgra");
    assert_eq!(
        (setup.queries[2].1, setup.queries[2].2),
        (
            0,
            Query::CodeActions {
                end: "let value = 1;\nvalue + 1".len()
            }
        )
    );
    // The menu shows by the cursor.
    setup.answer(
        setup.queries[2].0,
        Answer::CodeActions(vec![code_action("Extract", "refactor.extract", None)]),
    );
    assert_eq!(action_menu(&mut setup).unwrap().len(), 1);
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
    // A paragraph of one line goes to the blank line just after it, as
    // Vim's does, and a count goes on.
    let mut short = Setup::new("a\n\nb\n\nc\n");
    short.keys("}");
    assert_eq!(short.cursor(), (1, 0));
    short.keys("gg2}");
    assert_eq!(short.cursor(), (3, 0));
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
    setup.editor.click(1, 5, &mut setup.copy);
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

/// The copy given the agent hunks of chat 3's change from `base` to the
/// text now, in turn 1, as the daemon would send them: without their
/// bases.
fn agents_against(setup: &mut Setup, base: &str) {
    let text = setup.copy.text().to_string();
    let mut hunks = clackatoo_text::AgentHunks::new();
    let maker = clackatoo_text::Maker { chat: 3, turn: 1 };
    let edit = clackatoo_text::Edit::between(base, &text);
    hunks.add(&edit, (base, &text), (maker, &|_| true), &mut 1);
    let hunks = (hunks.all().iter())
        .map(|hunk| clackatoo_text::AgentHunk {
            base: String::new(),
            ..hunk.clone()
        })
        .collect();
    setup.copy.update(BufferUpdate::Agents {
        version: setup.daemon.version(),
        hunks,
        chats: vec![clackatoo_protocol::ChatName {
            chat: clackatoo_protocol::TerminalId(3),
            name: "Claude Code".into(),
        }],
    });
}

#[test]
fn an_agents_lines_not_reviewed_are_coloured_in_a_column_of_their_own() {
    let base = "a\nb\nc\nd\n";
    let mut setup = Setup::new("a\nB\nc\nd\n");
    setup.hunks_against(base);
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    assert_eq!(
        rows[1].text(),
        "▎  1 B",
        "no agents' column with no agents' lines"
    );
    agents_against(&mut setup, base);
    setup.editor.settle(&setup.copy);
    let (rows, cursor) = setup.editor.scene();
    // Right of git's sign, in the chat's colour.
    assert_eq!(rows[1].text(), "▎▍  1 B");
    assert_eq!(rows[0].text(), "    1 a");
    let cell = rows[1].cells()[1];
    assert_eq!(rows[1].style(cell).fg, crate::colors::agent(3));
    assert_eq!(cursor.unwrap().col, 6);
    // It moves with typing, and stays until the daemon says it's gone.
    setup.keys("ggOtop<Esc>");
    setup.editor.settle(&setup.copy);
    assert!(setup.editor.scene().0[2].text().starts_with("▎▍"));
    setup.copy.update(BufferUpdate::Agents {
        version: setup.daemon.version(),
        hunks: Vec::new(),
        chats: Vec::new(),
    });
    setup.editor.settle(&setup.copy);
    assert!(!setup.editor.scene().0[2].text().contains('▍'));
    // Turned off, there's no column.
    setup.keys("ggdd");
    agents_against(&mut setup, base);
    setup.set(|options| options.agentsigns = false);
    setup.editor.settle(&setup.copy);
    assert!(!setup.editor.scene().0[1].text().contains('▍'));
}

#[test]
fn blame_says_which_chat_and_turn_made_each_line() {
    let mut setup = Setup::new("a\nB\nc\n");
    agents_against(&mut setup, "a\nb\nc\n");
    setup.editor.toggle_blame();
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    assert_eq!(rows[1].text(), "▍  1 B  Claude Code, turn 2");
    assert_eq!(rows[0].text(), "   1 a");
    setup.editor.toggle_blame();
    setup.editor.settle(&setup.copy);
    assert_eq!(setup.editor.scene().0[1].text(), "▍  1 B");
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

impl Setup {
    /// Tells the editor its language server completes, asked by `.`, and
    /// helps with calls, asked by `(` and `,`, and again by `)`.
    fn server(&mut self) {
        self.copy.update(BufferUpdate::Features(ServerFeatures {
            completion: Some(vec!['.']),
            signature: Some(vec!['(', ',']),
            signature_again: vec![')'],
            prepare_rename: false,
            uses: false,
            inlay_hints: false,
        }));
    }

    /// Lets time pass, so what's due to be asked is.
    fn tick(&mut self) {
        let later = Instant::now() + Duration::from_secs(5);
        self.editor.tick(later, &mut self.copy);
        self.serve();
    }

    /// The last query sent, with its number and place.
    fn last_query(&self) -> (u64, usize, Query) {
        *self.queries.last().expect("a query")
    }

    /// The last completion query sent, with its number and place.
    fn last_completion(&self) -> (u64, usize, Query) {
        *(self.queries.iter().rev())
            .find(|query| matches!(query.2, Query::Completion(_)))
            .expect("a completion query")
    }

    /// The labels the completion menu shows, if it's shown.
    fn menu(&mut self) -> Option<Vec<String>> {
        self.editor.settle(&self.copy);
        let view = self.editor.completion.as_ref()?.menu()?;
        let labels = (view.lines.iter())
            .map(|line| line.text().split_whitespace().next().unwrap().to_owned())
            .collect();
        Some(labels)
    }
}

/// An item to complete with, `label`, replacing the word from `start`.
fn item(label: &str, start: usize) -> CompletionItem {
    CompletionItem {
        label: label.to_owned(),
        kind: 6,
        detail: String::new(),
        filter: None,
        sort: None,
        preselect: false,
        start: Some(start),
        text: label.to_owned(),
        snippet: false,
        more: Vec::new(),
    }
}

fn completions(items: Vec<CompletionItem>, incomplete: bool) -> Answer {
    Answer::Completions { items, incomplete }
}

#[test]
fn typing_a_word_asks_what_could_finish_it_once_typing_pauses() {
    let mut setup = Setup::new("alpha\n");
    setup.server();
    // With room for the documentation beside the menu.
    setup.editor.resize(Size::new(80, 10).unwrap());
    setup.keys("oal");
    assert!(setup.queries.is_empty(), "not while typing");
    setup.tick();
    let (asked, at, query) = setup.last_query();
    assert_eq!((at, query), (8, Query::Completion(Trigger::Invoked)));
    let items = vec![item("alpha", 6), item("almost", 6), item("beta", 6)];
    setup.answer(asked, completions(items, false));
    assert_eq!(setup.menu().unwrap(), ["almost", "alpha"]);
    // The first is picked, and its documentation asked for once picking
    // pauses.
    let resolve = |item| Query::Resolve { list: asked, item };
    assert!(matches!(setup.last_query().2, Query::Completion(_)));
    setup.tick();
    assert_eq!(setup.last_query().2, resolve(1));

    // Moving to the next asks for its; it's shown beside the menu. Moving
    // on quickly asks only about where it stops.
    let before = setup.queries.len();
    setup.keys("<C-n><C-n><C-n>");
    setup.tick();
    assert_eq!(setup.queries.len(), before + 1);
    let (resolving, _, query) = setup.last_query();
    assert_eq!(query, resolve(0));
    let documented = Answer::Resolved {
        detail: Some("fn alpha()".into()),
        documentation: Some(("Nearly **there**.".into(), true)),
    };
    setup.answer(resolving, documented);
    setup.editor.settle(&setup.copy);
    let panels = setup.editor.panels();
    assert_eq!(panels.len(), 2, "the menu and its documentation");
    let docs: Vec<String> = panels[1]
        .lines
        .iter()
        .map(super::panel::Line::text)
        .collect();
    assert_eq!(docs, ["fn alpha()", "", "Nearly there."]);

    // More typed narrows it down, without asking the server again for
    // completions, though it is for the documentation of what's picked
    // now.
    setup.keys("m");
    assert_eq!(setup.menu().unwrap(), ["almost"]);
    setup.tick();
    assert_eq!(setup.last_query().2, resolve(1));
    let asked_so_far = setup.queries.len();
    setup.tick();
    assert_eq!(setup.queries.len(), asked_so_far);

    // Ctrl+Y puts it in, as one undo step with what was typed.
    setup.keys("<C-y>");
    assert_eq!(setup.text(), "alpha\nalmost\n");
    assert_eq!(setup.cursor(), (1, 6));
    assert_eq!(setup.menu(), None);
    setup.keys("<Esc>u");
    assert_eq!(setup.text(), "alpha\n");
}

#[test]
fn accepting_makes_an_import_with_it_in_one_undo_step_and_dot_types_it_again() {
    let text = "fn main() {\n    \n}\n";
    let mut setup = Setup::new(text);
    setup.server();
    setup.keys("jAImp");
    setup.tick();
    let mut imported = item("Imported", "fn main() {\n    ".len());
    imported.more = vec![TextChange {
        start: 0,
        end: 0,
        text: "use elsewhere::Imported;\n".into(),
    }];
    setup.answer(setup.last_query().0, completions(vec![imported], false));
    setup.keys("<C-y>");
    assert_eq!(
        setup.text(),
        "use elsewhere::Imported;\nfn main() {\n    Imported\n}\n"
    );
    assert_eq!(setup.cursor(), (2, 12));
    setup.keys("<Esc>u");
    assert_eq!(setup.text(), text);

    // `.` types what was put in, in place of what was typed.
    let mut setup = Setup::new("\n\n");
    setup.server();
    setup.keys("ial");
    setup.tick();
    let mut call = item("call", 0);
    call.text = "call(${1:a})$0".into();
    call.snippet = true;
    setup.answer(setup.last_query().0, completions(vec![call], false));
    setup.keys("<C-y>");
    assert_eq!(setup.text(), "call(a)\n\n");
    assert_eq!(setup.cursor(), (0, 6), "past the first place to type");
    setup.keys("<Esc>j.");
    assert_eq!(setup.text(), "call(a)\ncall(a)\n");
}

#[test]
fn a_character_the_server_names_asks_at_once_and_more_is_asked_for_when_not_all() {
    let mut setup = Setup::new("x\n");
    setup.server();
    setup.keys("A.");
    let (first, at, query) = setup.last_query();
    assert_eq!((at, query), (2, Query::Completion(Trigger::Character('.'))));
    setup.answer(
        first,
        completions(vec![item("len", 2), item("last", 2)], true),
    );
    assert_eq!(setup.menu().unwrap(), ["last", "len"]);
    // Not all there is: typing asks again, once it pauses, keeping the
    // menu meanwhile.
    setup.keys("l");
    setup.tick();
    let (again, at, query) = setup.last_completion();
    assert_eq!((at, query), (3, Query::Completion(Trigger::Again)));
    assert_eq!(setup.menu().unwrap(), ["last", "len"]);
    // An answer to an older query is dropped.
    setup.answer(first, completions(vec![item("lost", 2)], false));
    assert_eq!(setup.menu().unwrap(), ["last", "len"]);
    setup.answer(again, completions(vec![item("last", 2)], false));
    assert_eq!(setup.menu().unwrap(), ["last"]);

    // Off, typing asks nothing, but Ctrl+Space still does; nothing to
    // complete is said, when asked for.
    setup.set(|options| options.completion = false);
    let before = setup.queries.len();
    setup.keys("<Esc>o.ab");
    setup.tick();
    assert!(
        (setup.queries[before..].iter()).all(|query| !matches!(query.2, Query::Completion(_))),
        "{:?}",
        &setup.queries[before..]
    );
    let before = setup.queries.len();
    setup.keys("<C-Space>");
    assert_eq!(setup.queries.len(), before + 1);
    setup.answer(setup.last_query().0, Answer::Nothing);
    assert_eq!(setup.editor.status.as_deref(), Some("no completions"));
}

#[test]
fn the_menu_closes_on_ctrl_e_esc_and_moving_away() {
    let mut setup = Setup::new("alpha\n");
    setup.server();
    let open = |setup: &mut Setup| {
        setup.keys("<C-n>");
        let (asked, at, query) = setup.last_query();
        assert_eq!(query, Query::Completion(Trigger::Invoked));
        let start = setup.copy.text().to_string()[..at]
            .rfind('\n')
            .map_or(0, |at| at + 1);
        setup.answer(asked, completions(vec![item("alpha", start)], false));
        assert!(setup.menu().is_some());
    };
    setup.keys("oal");
    open(&mut setup);
    setup.keys("<C-e>");
    assert_eq!(setup.menu(), None);
    assert_eq!(setup.editor.mode, Mode::Insert);
    open(&mut setup);
    setup.keys("<Left>");
    assert_eq!(setup.menu(), None);
    open(&mut setup);
    setup.keys("<Esc>");
    assert_eq!(setup.menu(), None);
    assert_eq!(setup.editor.mode, Mode::Normal);

    // An answer once the cursor has left the word isn't shown.
    setup.keys("A<C-n>");
    let asked = setup.last_query().0;
    setup.keys(" ");
    setup.answer(asked, completions(vec![item("alpha", 6)], false));
    assert_eq!(setup.menu(), None);
    assert_eq!(setup.text(), "alpha\nal \n");
}

#[test]
fn enter_and_tab_accept_as_the_options_say() {
    let ready = |change: fn(&mut Options)| {
        let mut setup = Setup::new("alpha\n");
        setup.server();
        setup.set(change);
        setup.keys("oal");
        setup.tick();
        let items = vec![item("alpha", 6), item("almost", 6)];
        setup.answer(setup.last_query().0, completions(items, false));
        setup
    };
    // By default, Enter accepts only an item moved to.
    let mut setup = ready(|_| {});
    setup.keys("<CR>");
    assert_eq!(setup.text(), "alpha\nal\n\n");
    let mut setup = ready(|_| {});
    setup.keys("<C-n><CR>");
    assert_eq!(setup.text(), "alpha\nalpha\n");
    let mut setup = ready(|options| options.completeaccept = CompleteAccept::Enter);
    setup.keys("<CR>");
    assert_eq!(setup.text(), "alpha\nalmost\n");
    let mut setup = ready(|options| options.completeaccept = CompleteAccept::CtrlY);
    setup.keys("<Down><CR>");
    assert_eq!(setup.text(), "alpha\nal\n\n");
    // Tab indents, unless it's told to accept.
    let mut setup = ready(|_| {});
    setup.keys("<Tab>");
    assert_eq!(setup.text(), "alpha\nal  \n");
    let mut setup = ready(|options| options.completetab = true);
    setup.keys("<Down><Up><Tab>");
    assert_eq!(setup.text(), "alpha\nalmost\n");
}

#[test]
fn an_item_lands_where_it_should_past_others_edits() {
    let mut setup = Setup::new("alpha\n");
    setup.server();
    setup.keys("oal");
    setup.tick();
    let asked = setup.last_query().0;
    // An agent types at the file's start before the server answers, about
    // the text with that in.
    let agents = setup.daemon.agent(0, "zz ");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(agents);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    setup.answer(
        asked,
        completions(vec![item("almost", "zz alpha\n".len())], false),
    );
    // And again after it has.
    let agents = setup.daemon.agent(0, "y");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(agents);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    setup.keys("<C-y>");
    assert_eq!(setup.text(), "yzz alpha\nalmost\n");
}

#[test]
fn a_pick_isnt_lost_to_a_list_asked_for_again() {
    let mut setup = Setup::new("x\n");
    setup.server();
    setup.keys("A.");
    let first = setup.last_completion().0;
    let items = || vec![item("len", 2), item("last", 2), item("lent", 2)];
    setup.answer(first, completions(items(), true));
    // Asked again as `l` is typed, as rust-analyzer always needs; Ctrl+N
    // before that goes keeps the pick, and Enter accepts it.
    setup.keys("l<C-n>");
    let asked = setup.queries.len();
    setup.tick();
    assert!(
        (setup.queries[asked..].iter()).all(|query| !matches!(query.2, Query::Completion(_))),
        "not asked again once picked"
    );
    // One already on its way is dropped.
    setup.keys("<C-e><C-Space>");
    let invoked = setup.last_completion().0;
    setup.answer(invoked, completions(items(), true));
    setup.keys("e");
    setup.tick();
    let again = setup.last_completion().0;
    setup.keys("<C-n>");
    let picked = setup.menu().unwrap();
    setup.answer(
        again,
        completions(vec![item("lent", 2), item("len", 2)], true),
    );
    assert_eq!(setup.menu().unwrap(), picked);
    // A list asked for anew keeps the item picked in the one it replaces.
    setup.keys("<C-Space>");
    let anew = setup.last_completion().0;
    setup.answer(
        anew,
        completions(vec![item("lent", 2), item("len", 2)], false),
    );
    setup.keys("<CR>");
    assert_eq!(setup.text(), "x.lent\n");
}

#[test]
fn dot_types_a_snippet_again_in_one_undo_step() {
    let mut setup = Setup::new("x\n\ny\n");
    setup.server();
    setup.keys("jibl");
    setup.tick();
    let mut block = item("block", 2);
    block.text = "if {\n\t$0\n}".into();
    block.snippet = true;
    setup.answer(setup.last_completion().0, completions(vec![block], false));
    setup.keys("<C-y>");
    // Its tab as Tab would put it in, with `expandtab`.
    assert_eq!(setup.text(), "x\nif {\n    \n}\ny\n");
    assert_eq!(setup.cursor(), (2, 4), "inside, past the tab");
    setup.keys("<Esc>jj0.");
    assert_eq!(setup.text(), "x\nif {\n    \n}\nif {\n    \n}y\n");
    setup.keys("u");
    assert_eq!(setup.text(), "x\nif {\n    \n}\ny\n", "one step");
}

#[test]
fn a_word_shorter_than_it_was_asked_for_is_asked_for_again() {
    let mut setup = Setup::new("\n");
    setup.server();
    setup.keys("ipri");
    setup.tick();
    let items = vec![item("print", 0), item("private", 0)];
    setup.answer(setup.last_completion().0, completions(items, false));
    assert_eq!(setup.menu().unwrap(), ["print", "private"]);
    // Back to `p`, which the server may have more for.
    setup.keys("<BS><BS>");
    assert_eq!(setup.menu(), None);
    setup.tick();
    let (asked, at, query) = setup.last_completion();
    assert_eq!((at, query), (1, Query::Completion(Trigger::Invoked)));
    setup.answer(asked, completions(vec![item("print", 0)], false));
    // Typed past all it gave: closed, and the next key asks again.
    setup.keys("u");
    assert_eq!(setup.menu(), None);
    let before = setup.queries.len();
    setup.keys("s");
    setup.tick();
    assert_eq!(setup.queries.len(), before + 1);
    assert_eq!(setup.last_completion().1, 3);
}

#[test]
fn an_item_whose_edits_dont_fit_keeps_the_menu_and_says_so() {
    let mut setup = Setup::new("abc\n");
    setup.server();
    setup.keys("A x");
    setup.tick();
    let mut broken = item("xyz", 4);
    broken.more = vec![
        TextChange {
            start: 0,
            end: 2,
            text: "1".into(),
        },
        TextChange {
            start: 1,
            end: 3,
            text: "2".into(),
        },
    ];
    setup.answer(setup.last_completion().0, completions(vec![broken], false));
    setup.keys("<C-y>");
    assert_eq!(setup.text(), "abc x\n");
    assert!(setup.menu().is_some());
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("the language server's edits for it don't fit")
    );
}

#[test]
fn a_calls_signature_shows_over_the_cursor_as_its_arguments_are_typed() {
    let mut setup = Setup::new("\n\n\n");
    setup.server();
    setup.keys("jjif(");
    let (asked, at, query) = setup.last_query();
    let help = |trigger, retrigger| Query::SignatureHelp { trigger, retrigger };
    assert_eq!((at, query), (4, help(Trigger::Character('('), false)));
    let label = "fn f(a: u32, b: u32)";
    let signature = |parameter: &str| Answer::Signature {
        label: label.into(),
        parameter: label
            .find(parameter)
            .map(|start| (start, start + parameter.len())),
        index: 0,
        count: 1,
    };
    setup.answer(asked, signature("a: u32"));
    let bold = |setup: &mut Setup| -> Option<String> {
        setup.editor.settle(&setup.copy);
        let panel = setup.editor.signature.as_ref()?.view()?;
        assert_eq!(panel.placed.top, 0, "over the cursor's line");
        let runs = panel.lines.iter().flat_map(|line| &line.runs);
        (runs.filter(|run| run.ink == Ink::Bold))
            .map(|run| run.text.clone())
            .next()
    };
    assert_eq!(bold(&mut setup).as_deref(), Some("a: u32"));
    // A comma asks again, and the next argument's in bold.
    setup.keys("1,");
    let (asked, _, query) = setup.last_query();
    assert_eq!(query, help(Trigger::Character(','), true));
    setup.answer(asked, signature("b: u32"));
    assert_eq!(bold(&mut setup).as_deref(), Some("b: u32"));

    // A word typed meanwhile asks what could finish it, and about the
    // call again, once typing pauses: the menu goes under the cursor, the
    // signature over it.
    setup.keys("x");
    setup.tick();
    let asked: Vec<Query> = setup
        .queries
        .iter()
        .rev()
        .take(2)
        .map(|query| query.2)
        .collect();
    assert!(
        asked.contains(&Query::Completion(Trigger::Invoked)),
        "{asked:?}"
    );
    assert!(asked.contains(&help(Trigger::Again, true)), "{asked:?}");
    let completing = (setup.queries.iter().rev())
        .find(|query| query.2 == Query::Completion(Trigger::Invoked))
        .unwrap()
        .0;
    setup.answer(
        completing,
        completions(vec![item("xs", "\n\nf(1,".len())], false),
    );
    setup.editor.settle(&setup.copy);
    let menu = setup.editor.completion.as_ref().unwrap().placed().unwrap();
    assert!(menu.top > 2, "under the cursor's line");
    assert_eq!(bold(&mut setup).as_deref(), Some("b: u32"));

    // Out of the call, the server says there's none, and it closes.
    setup.keys("<C-e>)");
    let (asked, _, query) = setup.last_query();
    assert_eq!(query, help(Trigger::Character(')'), true));
    setup.answer(asked, Answer::Nothing);
    assert_eq!(bold(&mut setup), None);
    // Leaving insert mode closes it.
    setup.keys("f(");
    setup.answer(setup.last_query().0, signature("a: u32"));
    assert!(bold(&mut setup).is_some());
    setup.keys("<Esc>");
    assert!(setup.editor.signature.is_none());
}

// --- Pieces of code, from the grammar -----------------------------------------

/// The piece of `text` that's `whole`, with `inner` in it as its insides.
fn piece_of(text: &str, whole: &str, inner: Option<&str>) -> Piece {
    let start = text.find(whole).expect("the piece is in the text");
    let inner = inner.map(|inner| {
        let at = start + whole.find(inner).expect("its insides are in it");
        (at, at + inner.len())
    });
    Piece {
        start,
        end: start + whole.len(),
        inner,
    }
}

const TWO: &str = "fn one(a, bb) {\n    two(a);\n}\n\nfn three() {\n    x;\n}";

fn one(text: &str) -> Piece {
    piece_of(text, "fn one(a, bb) {\n    two(a);\n}", Some("two(a);"))
}

#[test]
fn a_function_is_deleted_once_the_grammar_says_where_it_is() {
    let mut setup = Setup::new(TWO);
    setup.keys("jdaf");
    let (asked, at, query) = setup.last_query();
    assert_eq!(query, Query::Pieces(Construct::Function));
    assert_eq!(at, TWO.find("    two").unwrap());
    assert_eq!(setup.text(), TWO, "not until it answers");
    setup.answer(asked, Answer::Pieces(vec![one(TWO)]));
    assert_eq!(setup.text(), "\n\nfn three() {\n    x;\n}");
    assert_eq!(setup.register().text, "fn one(a, bb) {\n    two(a);\n}");
    assert_eq!(setup.cursor(), (0, 0));
}

#[test]
fn keys_typed_while_waiting_are_read_after_the_answer() {
    let mut setup = Setup::new(TWO);
    setup.keys("jdafiy<Esc>");
    assert_eq!(setup.text(), TWO);
    assert_eq!(setup.editor.mode, Mode::Normal, "the i is held");
    setup.answer(setup.last_query().0, Answer::Pieces(vec![one(TWO)]));
    assert_eq!(setup.text(), "y\n\nfn three() {\n    x;\n}");
    assert_eq!(setup.editor.mode, Mode::Normal);
    // A command in the keys held waits in turn, and the keys after it.
    setup.keys("Gdafx");
    let text = setup.copy.text().to_string();
    let three = piece_of(&text, "fn three() {\n    x;\n}", Some("x;"));
    setup.answer(setup.last_query().0, Answer::Pieces(vec![three]));
    assert_eq!(setup.text(), "y\n\n", "the x had nothing to delete");
}

#[test]
fn change_inside_a_function_and_dot_does_it_again() {
    let mut setup = Setup::new(TWO);
    setup.keys("jcif");
    setup.answer(setup.last_query().0, Answer::Pieces(vec![one(TWO)]));
    assert_eq!(setup.editor.mode, Mode::Insert);
    setup.keys("y();<Esc>");
    let text = "fn one(a, bb) {\n    y();\n}\n\nfn three() {\n    x;\n}";
    assert_eq!(setup.text(), text);
    // `.` asks again, where the cursor is, and types the same once it's
    // answered.
    setup.keys("G.");
    let (asked, at, query) = setup.last_query();
    assert_eq!(
        (at, query),
        (text.len() - 1, Query::Pieces(Construct::Function))
    );
    assert_eq!(setup.text(), text, "not until it answers");
    let three = piece_of(text, "fn three() {\n    x;\n}", Some("x;"));
    setup.answer(asked, Answer::Pieces(vec![three]));
    assert_eq!(
        setup.text(),
        "fn one(a, bb) {\n    y();\n}\n\nfn three() {\n    y();\n}"
    );
    assert_eq!(setup.editor.mode, Mode::Normal);
}

#[test]
fn arguments_are_copied_and_changed_with_their_commas() {
    let mut setup = Setup::new(TWO);
    setup.keys("fbyaa");
    let argument = piece_of(TWO, ", bb", Some("bb"));
    setup.answer(setup.last_query().0, Answer::Pieces(vec![argument]));
    assert_eq!(setup.register().text, ", bb");
    assert_eq!(setup.text(), TWO);
    setup.keys("cia");
    setup.answer(setup.last_query().0, Answer::Pieces(vec![argument]));
    setup.keys("c<Esc>");
    assert!(setup.text().starts_with("fn one(a, c) {"));
}

#[test]
fn af_again_in_visual_mode_grows_to_the_function_around() {
    let text = "fn outer() {\n    fn inner() {\n        z;\n    }\n}";
    let inner = piece_of(text, "fn inner() {\n        z;\n    }", Some("z;"));
    let outer = piece_of(text, text, Some("fn inner() {\n        z;\n    }"));
    let anchor = |setup: &Setup| setup.editor.anchor.map(|at| (at.line, at.col));
    let mut setup = Setup::new(text);
    setup.keys("jjvaf");
    let (asked, at, _) = setup.last_query();
    assert_eq!(at, text.find("        z").unwrap());
    setup.answer(asked, Answer::Pieces(vec![inner, outer]));
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Characters));
    assert_eq!(anchor(&setup), Some((1, 4)));
    assert_eq!(setup.cursor(), (3, 4));
    // Asked about the selection's start, and the next out picked.
    setup.keys("af");
    let (asked, at, _) = setup.last_query();
    assert_eq!(at, text.find("fn inner").unwrap());
    setup.answer(asked, Answer::Pieces(vec![inner, outer]));
    assert_eq!(anchor(&setup), Some((0, 0)));
    assert_eq!(setup.cursor(), (4, 0));
    // And `if` within it, then out a step.
    setup.keys("<Esc>ggjjvif");
    setup.answer(setup.last_query().0, Answer::Pieces(vec![inner, outer]));
    assert_eq!(anchor(&setup), Some((2, 8)));
    setup.keys("if");
    setup.answer(setup.last_query().0, Answer::Pieces(vec![inner, outer]));
    assert_eq!(anchor(&setup), Some((1, 4)));
    setup.keys("d");
    assert_eq!(setup.text(), "fn outer() {\n    \n}");
}

#[test]
fn a_count_picks_the_piece_that_many_out() {
    let text = "fn outer() {\n    fn inner() {\n        z;\n    }\n}";
    let inner = piece_of(text, "fn inner() {\n        z;\n    }", Some("z;"));
    let outer = piece_of(text, text, Some("fn inner() {\n        z;\n    }"));
    let mut setup = Setup::new(text);
    setup.keys("jjy2af");
    setup.answer(setup.last_query().0, Answer::Pieces(vec![inner, outer]));
    assert_eq!(setup.register().text, text);
}

#[test]
fn bracket_f_goes_to_the_next_function_and_is_a_jump() {
    let mut setup = Setup::new(TWO);
    setup.keys("]f");
    let (asked, at, query) = setup.last_query();
    let starts = |backward, count| Query::Starts {
        construct: Construct::Function,
        backward,
        count,
    };
    assert_eq!((at, query), (0, starts(false, 1)));
    let three = piece_of(TWO, "fn three() {\n    x;\n}", None);
    setup.answer(asked, Answer::Pieces(vec![three]));
    assert_eq!(setup.cursor(), (4, 0));
    assert_eq!(setup.editor.take_left(), Some(0), "a jump");
    // A count asks for that many, and goes as far as there are.
    setup.keys("j3[f");
    let (asked, _, query) = setup.last_query();
    assert_eq!(query, starts(true, 3));
    let one = piece_of(TWO, "fn one", None);
    setup.answer(asked, Answer::Pieces(vec![three, one]));
    assert_eq!(setup.cursor(), (0, 0));
    // `]C` and `[C` are classes'.
    setup.keys("]C");
    assert_eq!(
        setup.last_query().2,
        Query::Starts {
            construct: Construct::Class,
            backward: false,
            count: 1,
        }
    );
    setup.answer(setup.last_query().0, Answer::Pieces(Vec::new()));
    assert_eq!(setup.editor.status.as_deref(), Some("no class after here"));
}

#[test]
fn without_a_grammar_or_a_piece_the_bar_says_so() {
    let mut setup = Setup::new(TWO);
    setup.keys("daf");
    setup.answer(setup.last_query().0, Answer::Nothing);
    assert_eq!(setup.text(), TWO);
    assert!(
        (setup.editor.status.as_deref()).is_some_and(|status| status.contains("no grammar")),
        "{:?}",
        setup.editor.status
    );
    setup.keys("dac");
    setup.answer(setup.last_query().0, Answer::Pieces(Vec::new()));
    assert_eq!(setup.editor.status.as_deref(), Some("no class here"));
    setup.keys("dia");
    setup.answer(setup.last_query().0, Answer::Failed("it broke".into()));
    assert_eq!(setup.editor.status.as_deref(), Some("it broke"));
    assert_eq!(setup.text(), TWO);
}

#[test]
fn a_new_snapshot_while_waiting_gives_up_and_drops_the_keys() {
    let mut setup = Setup::new(TWO);
    setup.keys("dafx");
    let mut heard = setup.copy.update(setup.daemon.snapshot());
    setup.editor.heard(&mut heard, true, &mut setup.copy);
    assert!(!setup.editor.waiting_for_grammar());
    assert!(setup.editor.take_gave_up(), "for the desk to drop its keys");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("the file was read again: nothing was done")
    );
    // A late answer does nothing.
    setup.answer(setup.last_query().0, Answer::Pieces(vec![one(TWO)]));
    assert_eq!(setup.text(), TWO, "nor did the x");
}

#[test]
fn an_answer_too_late_does_nothing_and_drops_the_keys() {
    let mut setup = Setup::new(TWO);
    setup.keys("jcifreturn x");
    assert!(setup.editor.waiting_for_grammar());
    assert!(!setup.editor.waited_too_long(Instant::now()));
    setup.editor.age_awaiting(Duration::from_secs(2));
    assert!(setup.editor.waited_too_long(Instant::now()));
    setup.answer(setup.last_query().0, Answer::Pieces(vec![one(TWO)]));
    assert_eq!(setup.text(), TWO, "not cif, and no keys read as commands");
    assert_eq!(setup.editor.mode, Mode::Normal);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("the grammar took too long: nothing was done")
    );
    assert!(setup.editor.take_gave_up());
}

#[test]
fn a_command_waiting_too_long_is_given_up_on_with_no_key_and_none_is_eaten() {
    let mut setup = Setup::new(TWO);
    setup.keys("jdaf");
    let deadline = setup.editor.wakes().expect("woken to give up");
    assert!(!setup.editor.tick(
        deadline.checked_sub(Duration::from_millis(1)).unwrap(),
        &mut setup.copy
    ));
    assert!(setup.editor.tick(deadline, &mut setup.copy));
    assert!(!setup.editor.waiting_for_grammar());
    assert!(setup.editor.take_gave_up());
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("the grammar took too long: nothing was done")
    );
    // The next key is had, and the bar says what it did.
    setup.keys("x");
    assert_eq!(setup.text(), TWO.replacen("    two", "   two", 1));
    assert_eq!(setup.editor.status, None);
}

#[test]
fn a_cursor_moved_while_waiting_gives_up_but_others_edits_dont() {
    let mut setup = Setup::new(TWO);
    setup.keys("jdafx");
    // As a click moves it.
    setup.editor.cursor = Cursor::new(Point { line: 4, col: 0 });
    setup.answer(setup.last_query().0, Answer::Pieces(vec![one(TWO)]));
    assert_eq!(setup.text(), TWO);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("the cursor moved meanwhile: nothing was done")
    );
    // Someone else's edit before it moves the cursor and the place asked
    // about alike.
    setup.keys("ggjdaf");
    let edited = setup.daemon.agent(0, "// x\n");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(edited);
    setup.editor.heard(&mut heard, true, &mut setup.copy);
    let text = format!("// x\n{TWO}");
    setup.answer(setup.last_query().0, Answer::Pieces(vec![one(&text)]));
    assert_eq!(setup.text(), "// x\n\n\nfn three() {\n    x;\n}");
}

#[test]
fn bracket_f_in_visual_mode_asks_from_the_cursor() {
    let mut setup = Setup::new(TWO);
    setup.keys("vj]f");
    let (_, at, query) = setup.last_query();
    assert!(matches!(query, Query::Starts { .. }));
    assert_eq!(
        at,
        TWO.find("    two").unwrap(),
        "not the selection's start"
    );
}

#[test]
fn the_menu_of_code_actions_and_keys_held_for_the_grammar_get_along() {
    let mut setup = Setup::new(TWO);
    let actions = vec![code_action("Fix it", "quickfix", None)];
    setup.keys("gra");
    setup.answer(setup.last_query().0, Answer::CodeActions(actions.clone()));
    assert!(action_menu(&mut setup).is_some());
    // y isn't the menu's: it closes it, and the j waits with the yank.
    setup.keys("yafj");
    assert!(action_menu(&mut setup).is_none());
    setup.answer(setup.last_query().0, Answer::Pieces(vec![one(TWO)]));
    assert_eq!(setup.register().text, "fn one(a, bb) {\n    two(a);\n}");
    assert_eq!(setup.cursor(), (1, 0), "the j moved the cursor");
    // gra typed while waiting is held too, and asks once it's read.
    setup.keys("ggdafgra");
    let (pieces, _, query) = setup.last_query();
    assert_eq!(query, Query::Pieces(Construct::Function), "gra is held");
    setup.answer(pieces, Answer::Pieces(vec![one(TWO)]));
    let (list, _, query) = setup.last_query();
    assert!(matches!(query, Query::CodeActions { .. }), "{query:?}");
    setup.answer(list, Answer::CodeActions(actions));
    assert!(action_menu(&mut setup).is_some());
    assert_eq!(setup.text(), "\n\nfn three() {\n    x;\n}");
}

// --- Registers, macros and marks --------------------------------------------

#[test]
fn capital_registers_add_to_the_named_and_p_puts_them() {
    let mut setup = Setup::new("one\ntwo\nthree\n");
    setup.keys("\"ayyj\"Ayy");
    assert_eq!(setup.held('a').as_deref(), Some("one\ntwo\n"));
    setup.keys("G\"ap");
    assert_eq!(setup.text(), "one\ntwo\nthree\none\ntwo\n");
    // Characters added to lines go on a line of their own.
    setup.keys("gg\"Ayw");
    assert_eq!(setup.held('a').as_deref(), Some("one\ntwo\none\n"));
    // A register named to `x`, and a count with it either side.
    setup.keys("gg\"b2x");
    assert_eq!(setup.held('b').as_deref(), Some("on"));
    setup.keys("2\"b2P");
    assert_eq!(setup.text(), "onononone\ntwo\nthree\none\ntwo\n");
}

#[test]
fn deletes_push_down_the_numbered_registers_and_yanks_go_to_0() {
    let mut setup = Setup::new("a\nb\nc\nd\n");
    setup.keys("yyjddddx");
    assert_eq!(setup.held('0').as_deref(), Some("a\n"), "the yank");
    assert_eq!(setup.held('1').as_deref(), Some("c\n"));
    assert_eq!(setup.held('2').as_deref(), Some("b\n"));
    // `x` within a line goes to `-`, not `1`.
    assert_eq!(setup.held('-').as_deref(), Some("d"));
    assert_eq!(setup.text(), "a\n\n");
    // `"1p`, then `.` puts `"2`'s, going back through the deletes.
    setup.keys("gg\"1p.");
    assert_eq!(setup.text(), "a\nc\nb\n\n");
    // `0` keeps the yank through deletes.
    setup.keys("dd\"0P");
    assert_eq!(setup.text(), "a\nc\na\n\n");
}

#[test]
fn the_black_hole_takes_deletes_and_leaves_the_registers_alone() {
    let mut setup = Setup::new("keep\ngone\n");
    setup.keys("yyj\"_dd");
    assert_eq!(setup.text(), "keep\n");
    assert_eq!(setup.register().text, "keep\n");
    assert_eq!(setup.held('1'), None);
    setup.keys("p");
    assert_eq!(setup.text(), "keep\nkeep\n");
    // `"_p` puts nothing, and stops a macro, as an empty register does.
    setup.keys("\"_p");
    assert_eq!(setup.text(), "keep\nkeep\n");
    assert_eq!(setup.editor.status.as_deref(), Some("register _ is empty"));
}

#[test]
fn registers_in_visual_mode_and_p_over_a_selection() {
    let mut setup = Setup::new("one two\nthree\n");
    setup.keys("v$\"cy");
    assert_eq!(setup.held('c').as_deref(), Some("one two"));
    // `p` over a word puts in its place, keeping the word.
    setup.keys("wyiwbviwp");
    assert_eq!(setup.text(), "two two\nthree\n");
    assert_eq!(setup.register().text, "one");
    // `P` keeps what it puts, so it can go over one selection after
    // another; whole lines go in place of whole lines.
    setup.keys("yyjVP");
    assert_eq!(setup.text(), "two two\ntwo two\n");
    assert_eq!(setup.register().text, "two two\n");
    // Characters in place of lines go on a line of their own, and lines in
    // place of characters break the line around them.
    setup.keys("ggV\"cp");
    assert_eq!(setup.text(), "one two\ntwo two\n");
    setup.keys("jwviwp");
    assert_eq!(setup.text(), "one two\ntwo \ntwo two\n\n");
}

#[test]
fn the_clipboard_goes_along_with_the_unnamed_register_as_clipboard_says() {
    let mut setup = Setup::new("a\nb\n");
    setup.keys("yy");
    assert_eq!(
        setup.shared.registers.take_to_system(),
        [("a\n".to_owned(), crate::pane::Copy::Clipboard)]
    );
    // Copied elsewhere, `p` puts it, and so does `"+p`.
    let copied = "pasted".to_owned();
    setup.shared.registers.asking_system();
    (setup.shared.registers).heard_system(crate::pane::Copy::Clipboard, copied);
    setup.keys("jp\"+p");
    assert_eq!(setup.text(), "a\nbpastedpasted\n");
    (setup.shared.registers).set_sync(crate::options::Clipboard::Off);
    setup.keys("dd");
    assert!(setup.shared.registers.take_to_system().is_empty());
    assert_eq!(setup.register().text, "bpastedpasted\n");
    setup.keys("\"*yy");
    assert_eq!(
        setup.shared.registers.take_to_system(),
        [("a\n".to_owned(), crate::pane::Copy::Primary)]
    );
}

#[test]
fn nothing_deleted_keeps_the_registers() {
    let mut setup = Setup::new("kept\n\n");
    setup.keys("yyjD");
    assert_eq!(setup.register().text, "kept\n");
    setup.keys("C<Esc>\"_x");
    assert_eq!(setup.register().text, "kept\n");
    assert_eq!(setup.held('-'), None);
    assert_eq!(
        setup.shared.registers.take_to_system(),
        [("kept\n".to_owned(), crate::pane::Copy::Clipboard)],
        "the clipboard has only the yank"
    );
}

#[test]
fn marks_go_to_their_place_or_line_and_operators_act_up_to_them() {
    let mut setup = Setup::new("  one\n  two\n  three\n");
    setup.keys("wlmajjmb");
    setup.keys("`a");
    assert_eq!(setup.cursor(), (0, 3));
    setup.keys("'b");
    assert_eq!(setup.cursor(), (2, 2), "the line's first character");
    setup.keys("y'a");
    assert_eq!(setup.register().text, "  one\n  two\n  three\n");
    assert!(setup.register().lines);
    setup.keys("`bd`a");
    assert_eq!(setup.text(), "  ohree\n");
    setup.keys("'z");
    assert_eq!(setup.editor.status.as_deref(), Some("mark z isn't set"));
    // A mark in another file is said to be there.
    let text = clackatoo_text::Rope::from_str("x");
    let elsewhere = super::FilePlace::at(super::File::here("/src/other.rs"), &text, 0);
    setup.shared.marks.set('Q', elsewhere);
    setup.keys("d'Q");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("mark Q is in other.rs")
    );
    assert_eq!(setup.text(), "  ohree\n");
}

#[test]
fn ctrl_w_and_backspace_at_a_lines_start_join_it_to_the_line_before() {
    let mut setup = Setup::new("one\ntwo\nthree\n");
    setup.keys("ji<C-w><Esc>");
    assert_eq!(setup.text(), "onetwo\nthree\n");
    setup.keys("j0i<BS><Esc>");
    assert_eq!(setup.text(), "onetwothree\n");
}

#[test]
fn hidden_starts_are_never_drawn_nor_gone_to_and_changing_a_line_keeps_them() {
    use crate::explorer::listing::{mark, mark_len};
    let (one, two) = (mark(1, 0), mark(1, 1));
    let mut setup = Setup::new(&format!("{one}alpha\n{two}beta/\n"));
    setup.set(|options| {
        options.number = false;
        options.relativenumber = false;
    });
    setup.editor.hide_starts(mark_len);
    setup.editor.settle(&setup.copy);
    let rows: Vec<String> = (setup.editor.scene().0.iter())
        .take(2)
        .map(|row| row.text())
        .collect();
    assert_eq!(rows, ["alpha", "beta/"]);
    setup.keys("$0x");
    assert_eq!(setup.cursor(), (0, 0));
    assert_eq!(setup.text(), format!("{one}lpha\n{two}beta/\n"));
    setup.keys("Ixy<Esc>Az<Esc>");
    assert_eq!(setup.text(), format!("{one}xylphaz\n{two}beta/\n"));
    setup.keys("ccgamma<Esc>");
    assert_eq!(
        setup.text(),
        format!("{one}gamma\n{two}beta/\n"),
        "kept, as an indent is"
    );
    setup.keys("ddp");
    assert_eq!(
        setup.text(),
        format!("{two}beta/\n{one}gamma\n"),
        "moved with its line"
    );
    setup.keys("ggd$");
    assert_eq!(setup.text(), format!("{two}\n{one}gamma\n"));
    // Typed into a line with nothing shown, it goes after what's hidden.
    setup.keys("ix<Esc>");
    assert_eq!(setup.text(), format!("{two}x\n{one}gamma\n"));
    setup.keys("u");
    // Backspace at the start of what's shown joins the line to the one
    // before, taking its mark, whole, into the middle of that line, which
    // a listing refuses; Ctrl+W and Ctrl+U there do nothing.
    setup.keys("ji<C-w><C-u><Esc>");
    assert_eq!(setup.text(), format!("{two}\n{one}gamma\n"));
    setup.keys("i<BS><Esc>");
    assert_eq!(setup.text(), format!("{two}{one}gamma\n"));
}
