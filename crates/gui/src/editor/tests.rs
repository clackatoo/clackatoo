//! An editor driven by keys, with its buffer's copy talking to a daemon as
//! the real one does.

use clackatoo_protocol::{BufferId, Size};

use super::copy::tests::Daemon;
use super::keys::Stroke;
use super::{BufferCopy, Editor, Mode, Register};

struct Setup {
    editor: Editor,
    copy: BufferCopy,
    daemon: Daemon,
    register: Register,
}

impl Setup {
    fn new(text: &str) -> Self {
        let daemon = Daemon::new(text);
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        let mut editor = Editor::new(BufferId(1));
        editor.resize(Size::new(40, 10).unwrap());
        Self {
            editor,
            copy,
            daemon,
            register: Register::default(),
        }
    }

    /// Types `keys`, Vim's way: `<Esc>`, `<CR>`, `<BS>`, `<Tab>`, `<C-r>`.
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
