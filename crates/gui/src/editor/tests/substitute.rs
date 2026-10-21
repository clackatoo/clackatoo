//! `:s`, `:g` and the editor's other commands for lines, their preview as
//! they're typed, and changing case, each as Vim does it (checked against
//! Neovim, case by case).

use clackatoo_protocol::ClientMessage;

use super::Setup;
use crate::render::HighlightKind;

/// Does `line`, typed after `:`, as the desk does, and serves the daemon.
fn ex(setup: &mut Setup, line: &str) -> Result<(), String> {
    let done = setup
        .editor
        .ex(line, &mut setup.copy, &mut setup.shared, false)
        .map(|_| ());
    setup.follow_marks();
    setup.serve();
    done
}

/// `ex`, which must work.
fn run(setup: &mut Setup, line: &str) {
    if let Err(why) = ex(setup, line) {
        panic!(":{line} failed: {why}");
    }
}

/// The text after `line` is done on `text`, with the cursor where `keys`
/// put it first.
fn after(text: &str, keys: &str, line: &str) -> String {
    let mut setup = Setup::new(text);
    setup.keys(keys);
    run(&mut setup, line);
    setup.text()
}

/// The rows drawn, as text, without the line numbers.
fn drawn(setup: &mut Setup) -> Vec<String> {
    setup.editor.settle(&setup.copy);
    let (rows, _) = setup.editor.scene();
    (rows.iter())
        .map(|row| {
            let text = row.text();
            text.trim_start()
                .trim_start_matches(|ch: char| ch.is_ascii_digit())
                .trim_start()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn ranges_are_read_as_vim_reads_them() {
    let lines = "1\n2\n3\n4\n5\n6\n";
    assert_eq!(after(lines, "j", "s/$/!/"), "1\n2!\n3\n4\n5\n6\n");
    assert_eq!(after(lines, "j", ".,.+2s/$/!/"), "1\n2!\n3!\n4!\n5\n6\n");
    assert_eq!(after(lines, "j", "s/$/!/ 3"), "1\n2!\n3!\n4!\n5\n6\n");
    assert_eq!(after(lines, "", "2;+1s/$/!/"), "1\n2!\n3!\n4\n5\n6\n");
    assert_eq!(after(lines, "", "5,$s/$/!/"), "1\n2\n3\n4\n5!\n6!\n");
    assert_eq!(after(lines, "", "%s/$/!/"), "1!\n2!\n3!\n4!\n5!\n6!\n");
    // Backwards is turned round, and line 0 is the first.
    assert_eq!(after(lines, "", "3,2s/$/!/"), "1\n2!\n3!\n4\n5\n6\n");
    assert_eq!(after(lines, "", "0s/$/!/"), "1!\n2\n3\n4\n5\n6\n");
    assert_eq!(
        after("1\nx\n3\ny\n5\n", "", "/x/,/y/s/$/!/"),
        "1\nx!\n3!\ny!\n5\n"
    );
    assert_eq!(after("x\n2\nx\n4\n", "3G", "?x?s/$/!/"), "x!\n2\nx\n4\n");
    assert_eq!(
        after(lines, "jjmajj", "'a,.s/$/!/"),
        "1\n2\n3!\n4!\n5!\n6\n"
    );
    let mut setup = Setup::new(lines);
    assert!(
        ex(&mut setup, "9s/a/b/")
            .unwrap_err()
            .contains("there's no line 9")
    );
    assert!(
        ex(&mut setup, "'zs/a/b/")
            .unwrap_err()
            .contains("mark z isn't set")
    );
    // A range alone goes to its last line, as in Vim.
    run(&mut setup, "1,2");
    assert_eq!(setup.cursor(), (1, 0));
    assert!(
        ex(&mut setup, "1,2frob")
            .unwrap_err()
            .contains("frob isn't a command")
    );
}

#[test]
fn a_selection_gives_its_lines_to_the_command_line() {
    let mut setup = Setup::new("a\na\na\na\n");
    setup.keys("jVj");
    let start = setup
        .editor
        .command_line(&setup.copy, &mut setup.shared.marks);
    assert_eq!(start, "'<,'>");
    assert!(!matches!(setup.editor.mode, crate::vim::Mode::Visual(_)));
    run(&mut setup, "'<,'>s/a/b/");
    assert_eq!(setup.text(), "a\nb\nb\na\n");
    assert_eq!(setup.cursor(), (2, 0));
    // Leaving visual mode keeps them, and edits move them.
    let mut setup = Setup::new("a\nb\nc\nd\n");
    setup.keys("jvj<Esc>ggOnew<Esc>");
    run(&mut setup, "'<,'>s/^/>/");
    assert_eq!(setup.text(), "new\na\n>b\n>c\nd\n");
}

#[test]
fn every_flag() {
    assert_eq!(after("a a\na\n", "", "%s/a/b/"), "b a\nb\n");
    assert_eq!(after("a a\na\n", "", "%s/a/b/g"), "b b\nb\n");
    // Case, as `ignorecase` and `smartcase` say, unless `i` or `I` says.
    assert_eq!(after("Foo foo\n", "", "s/foo/x/g"), "x x\n");
    assert_eq!(after("Foo foo\n", "", "s/Foo/x/g"), "x foo\n");
    assert_eq!(after("Foo foo\n", "", "s/foo/x/gI"), "Foo x\n");
    let mut setup = Setup::new("Foo foo\n");
    setup.set(|options| options.ignorecase = false);
    run(&mut setup, "s/foo/x/gi");
    assert_eq!(setup.text(), "x x\n");
    // `n` counts, changing nothing.
    let mut setup = Setup::new("a a\na\n");
    run(&mut setup, "%s/a/b/gn");
    assert_eq!(setup.text(), "a a\na\n");
    assert_eq!(setup.editor.status.as_deref(), Some("3 matches on 2 lines"));
    // `e` says nothing of no match; without it, it fails, stopping a macro.
    let mut setup = Setup::new("abc\n");
    run(&mut setup, "s/zzz/y/e");
    assert_eq!(
        ex(&mut setup, "s/zzz/y/"),
        Err("zzz isn't in the lines given".into())
    );
    assert!(
        ex(&mut setup, "s/a/b/z")
            .unwrap_err()
            .contains("isn't one of :s's flags")
    );
    // `&` keeps the last flags.
    assert_eq!(after("a a a\n", "", "s/a/b/"), "b a a\n");
    let mut setup = Setup::new("a a a\na a a\n");
    run(&mut setup, "s/a/b/g");
    run(&mut setup, "2s/a/c/&");
    assert_eq!(setup.text(), "b b b\nc c c\n");
    // `r`, with no pattern, takes the last search's, not the last `:s`'s.
    let mut setup = Setup::new("a c\na c\n");
    run(&mut setup, "s/a/b/");
    setup.keys("/c<CR>");
    run(&mut setup, "2&r");
    assert_eq!(setup.text(), "b c\na b\n");
    // A report of more than two, as Vim's `report` says.
    let mut setup = Setup::new("a\na\na\n");
    run(&mut setup, "%s/a/b/");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("3 substitutions on 3 lines")
    );
}

#[test]
fn the_replacements_specials() {
    let text = "foo bar\n";
    assert_eq!(
        after(text, "", "s/\\(foo\\) \\(bar\\)/\\u\\1-\\U\\2\\E!-&-\\0/"),
        "Foo-BAR!-foo bar-foo bar\n"
    );
    assert_eq!(
        after("HELLO world\n", "", "s/\\w\\+/\\L\\u&/g"),
        "Hello World\n"
    );
    assert_eq!(after("a,b\nc\n", "", "s/,/\\r/"), "a\nb\nc\n");
    assert_eq!(after("a,b\n", "", "s/,/\\t/"), "a\tb\n");
    assert_eq!(after("a/b\n", "", "s#/#|#"), "a|b\n");
    assert_eq!(after("abc\n", "", "s/b"), "ac\n");
    assert_eq!(after("a\nb\nc\n", "", "%s/\\n//"), "abc\n");
    assert_eq!(
        after("ab12 cd\n", "", "s/\\v(\\a+)(\\d{2})/\\2\\1/"),
        "12ab cd\n"
    );
    assert_eq!(
        after("cat concat cats cat\n", "", "s/\\<cat\\>/dog/g"),
        "dog concat cats dog\n"
    );
    // `~` is the last replacement, in a replacement and in a pattern.
    let mut setup = Setup::new("a b\na b\nab ab\n");
    run(&mut setup, "s/a/X/");
    run(&mut setup, "2s/b/~Y/");
    assert_eq!(setup.text(), "X b\na XY\nab ab\n");
    run(&mut setup, "2s/~/Z/");
    assert_eq!(setup.text(), "X b\na Z\nab ab\n");
    // An empty pattern is the last search's.
    let mut setup = Setup::new("abc abc\n");
    setup.keys("/b<CR>");
    run(&mut setup, "s//X/g");
    assert_eq!(setup.text(), "aXc aXc\n");
    // And `:s` makes its pattern the last search, for `n`.
    let mut setup = Setup::new("a b\nc b\nx a\n");
    run(&mut setup, "s/b/Y/");
    setup.keys("n");
    assert_eq!(setup.cursor(), (1, 2));
}

#[test]
fn ampersand_and_its_kin_make_the_last_substitute_again() {
    // `&` on the line, without the flags; `:&&` with them.
    let mut setup = Setup::new("a a a\na a\n");
    run(&mut setup, "s/a/b/g");
    run(&mut setup, "2&&");
    assert_eq!(setup.text(), "b b b\nb b\n");
    // `&` forgets them, as Vim does, so `:&&` after it keeps none.
    let mut setup = Setup::new("a a a\na a\na a\n");
    run(&mut setup, "s/a/b/g");
    setup.keys("j&");
    assert_eq!(setup.text(), "b b b\nb a\na a\n");
    run(&mut setup, "3&&");
    assert_eq!(setup.text(), "b b b\nb a\nb a\n");
    // `:s` alone, and `:&`, are `&`; with a count, that many lines.
    let mut setup = Setup::new("a a\na a\na a\n");
    run(&mut setup, "s/a/b/");
    run(&mut setup, "2s");
    assert_eq!(setup.text(), "b a\nb a\na a\n");
    run(&mut setup, "& 2");
    assert_eq!(setup.text(), "b a\nb b\nb a\n");
    // `g&` on every line, with the flags and the last search's pattern.
    let mut setup = Setup::new("a b\na b\n");
    run(&mut setup, "s/a/X/");
    setup.keys("/b<CR>g&");
    assert_eq!(setup.text(), "X X\na X\n");
    // `:~` with the last search's pattern.
    let mut setup = Setup::new("a b\na b b\n");
    run(&mut setup, "s/a/X/");
    setup.keys("/b<CR>");
    run(&mut setup, "2~");
    assert_eq!(setup.text(), "X b\na X b\n");
    // Nothing to make again yet.
    let mut setup = Setup::new("a\n");
    assert!(
        ex(&mut setup, "&&")
            .unwrap_err()
            .contains("no :s to do again")
    );
    setup.keys("&");
    assert!(setup.editor.take_failed());
    // `.` doesn't make `&` again, as in Vim, but the last change.
    let mut setup = Setup::new("a a\na a\n");
    setup.keys("x");
    run(&mut setup, "s/a/b/");
    setup.keys("j.");
    assert_eq!(setup.text(), " b\naa\n");
}

#[test]
fn a_substitute_is_one_edit_and_one_undo_step() {
    let mut setup = Setup::new("a a\nx\na\na a a\n");
    setup
        .editor
        .ex("%s/a/b/g", &mut setup.copy, &mut setup.shared, false)
        .unwrap();
    let edits = (setup.copy.take_outbox().into_iter())
        .filter(|message| matches!(message, ClientMessage::Edit { .. }))
        .count();
    assert_eq!(edits, 1);
    // What was taken is served now, as it would have been.
    setup = Setup::new("a a\nx\na\na a a\n");
    run(&mut setup, "%s/a/b/g");
    assert_eq!(setup.text(), "b b\nx\nb\nb b b\n");
    assert_eq!(setup.cursor(), (3, 0));
    setup.keys("u");
    assert_eq!(setup.text(), "a a\nx\na\na a a\n");
    setup.keys("<C-r>");
    assert_eq!(setup.text(), "b b\nx\nb\nb b b\n");
}

#[test]
fn confirming_asks_about_each_match_in_the_bar() {
    let mut setup = Setup::new("a a\na\na a\n");
    run(&mut setup, "%s/a/b/gc");
    assert!(setup.editor.prompting());
    assert_eq!(
        setup.editor.label(&setup.copy),
        "replace with b (y/n/a/q/l/^E/^Y)?"
    );
    assert_eq!(setup.cursor(), (0, 0));
    setup.keys("y");
    assert_eq!(setup.cursor(), (0, 2));
    setup.keys("n");
    assert_eq!(setup.cursor(), (1, 0));
    setup.keys("y");
    assert_eq!(setup.text(), "b a\nb\na a\n");
    // `a` does the rest, and all of it is one undo step.
    setup.keys("a");
    assert!(!setup.editor.prompting());
    assert_eq!(setup.text(), "b a\nb\nb b\n");
    setup.keys("u");
    assert_eq!(setup.text(), "a a\na\na a\n");
    // `l` does this one and stops; `q` and Esc stop.
    for (keys, text) in [
        ("nl", "a b\na\n"),
        ("yq", "b a\na\n"),
        ("y<Esc>", "b a\na\n"),
    ] {
        let mut setup = Setup::new("a a\na\n");
        run(&mut setup, "%s/a/b/gc");
        setup.keys(keys);
        assert!(!setup.editor.prompting(), "{keys}");
        assert_eq!(setup.text(), text, "{keys}");
    }
    // Someone else's edit meanwhile moves what's asked about.
    let mut setup = Setup::new("a a\n");
    run(&mut setup, "s/a/b/gc");
    let agent = setup.daemon.agent(0, "zz ");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(agent);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    setup.keys("yy");
    assert_eq!(setup.text(), "zz b b\n");
}

#[test]
fn confirming_shows_the_first_match_and_isnt_done_by_global() {
    let text = (0..100)
        .map(|line| format!("line {line}\n"))
        .collect::<Vec<_>>()
        .concat();
    let mut setup = Setup::new(&text);
    drawn(&mut setup);
    run(&mut setup, "%s/line 70/seventy/c");
    assert_eq!(setup.cursor(), (70, 0));
    assert!(
        setup.editor.top.line > 60,
        "in view: {}",
        setup.editor.top.line
    );
    assert_eq!(setup.highlighted(HighlightKind::Match).len(), 1);
    // An empty match at the file's start is replaced too.
    let mut setup = Setup::new("a\nb\n");
    run(&mut setup, "%s/^/# /gc");
    setup.keys("yy");
    assert_eq!(setup.text(), "# a\n# b\n");
    // Not under `:g`, which can't ask yet.
    let mut setup = Setup::new("x\n");
    assert!(
        ex(&mut setup, "g/x/s/x/y/c")
            .unwrap_err()
            .contains("can't be done by a :g")
    );
    assert_eq!(setup.text(), "x\n");
}

#[test]
fn a_range_alone_goes_to_its_line_as_a_jump() {
    let lines = "1\n2\n  3\n4\n5\n";
    let gone = |keys: &str, line: &str| {
        let mut setup = Setup::new(lines);
        setup.keys(keys);
        run(&mut setup, line);
        setup.cursor()
    };
    // As Neovim: to the line's start.
    assert_eq!(gone("", "3"), (2, 0));
    assert_eq!(gone("", "+3"), (3, 0));
    assert_eq!(gone("G", "-2"), (2, 0));
    assert_eq!(gone("", "$"), (4, 0));
    assert_eq!(gone("jjmagg", "'a"), (2, 0));
    // Past the end, the last; 0, the first.
    assert_eq!(gone("", "9"), (4, 0));
    assert_eq!(gone("G", "0"), (0, 0));
    let mut setup = Setup::new(lines);
    run(&mut setup, "4");
    assert_eq!(setup.editor.take_left(), Some(0), "a jump, for Ctrl+O");
    // Text after a line too far is still a mistake.
    assert!(
        ex(&mut setup, "9d")
            .unwrap_err()
            .contains("there's no line 9")
    );
}

#[test]
fn the_preview_shows_the_replacements_in_place_and_goes_on_esc() {
    let mut setup = Setup::new("one two\ntwo three\n");
    let before = drawn(&mut setup);
    setup
        .editor
        .preview(Some("%s/two/2/g"), &setup.copy, &setup.shared);
    assert_eq!(drawn(&mut setup)[..2], ["one 2", "2 three"]);
    // Lit as a search's match is.
    let lit = setup.highlighted(HighlightKind::Match);
    assert_eq!(lit.len(), 2);
    // The buffer isn't touched, and the daemon hears of nothing.
    assert_eq!(setup.copy.text().to_string(), "one two\ntwo three\n");
    assert!(setup.copy.take_outbox().is_empty());
    // Typing more shows more; a pattern alone lights its matches.
    setup
        .editor
        .preview(Some("%s/t"), &setup.copy, &setup.shared);
    assert_eq!(drawn(&mut setup), before);
    assert_eq!(setup.highlighted(HighlightKind::Match).len(), 3);
    // Esc closes the command line, and the preview goes.
    setup.editor.preview(None, &setup.copy, &setup.shared);
    assert_eq!(drawn(&mut setup), before);
    assert!(setup.highlighted(HighlightKind::Match).is_empty());
    assert_eq!(setup.text(), "one two\ntwo three\n");
    // Not with `inccommand=none`.
    setup.set(|options| options.inccommand = crate::options::InCommand::Off);
    setup
        .editor
        .preview(Some("%s/two/2/g"), &setup.copy, &setup.shared);
    assert_eq!(drawn(&mut setup), before);
}

#[test]
fn the_preview_goes_to_a_match_out_of_view_and_back() {
    let text = (0..100)
        .map(|line| format!("line {line}\n"))
        .collect::<Vec<_>>()
        .concat();
    let mut setup = Setup::new(&text);
    drawn(&mut setup);
    setup
        .editor
        .preview(Some("%s/line 70/seventy/"), &setup.copy, &setup.shared);
    assert!(drawn(&mut setup).iter().any(|row| row == "seventy"));
    assert_ne!(setup.editor.top.line, 0);
    setup.editor.preview(None, &setup.copy, &setup.shared);
    assert_eq!(setup.editor.top.line, 0);
    // Wrapped lines draw the replacement on the rows they wrap onto.
    let mut setup = Setup::new(&format!("{}x\n", "a ".repeat(30)));
    setup
        .editor
        .preview(Some("s/x/END/"), &setup.copy, &setup.shared);
    let rows = drawn(&mut setup);
    assert!(rows.iter().any(|row| row.ends_with("END")), "{rows:?}");
    let lit = setup.highlighted(HighlightKind::Match);
    assert_eq!(lit.len(), 1);
    assert!(lit[0].0 > 0, "on the row it wrapped onto: {lit:?}");
}

#[test]
fn global_does_its_command_on_each_line_that_matches() {
    assert_eq!(after("x1\ny\nx2\nx3\nz\n", "", "g/x/d"), "y\nz\n");
    assert_eq!(after("x1\ny\nx2\nz\n", "", "v/x/d"), "x1\nx2\n");
    assert_eq!(after("x1\ny\nx2\nz\n", "", "g!/x/d"), "x1\nx2\n");
    assert_eq!(after("1\n2\n3\n4\n", "", "g/^/m0"), "4\n3\n2\n1\n");
    assert_eq!(
        after("x1\ny\nx2\n", "", "g/x/normal Atext"),
        "x1text\ny\nx2text\n"
    );
    assert_eq!(after("x1 x\ny x\nx2\n", "", "g/x/s//y/"), "y1 x\ny y\ny2\n");
    assert_eq!(after("x\nx\nx\ny\n", "", "g/x/normal J"), "x x\nx y\n");
    assert_eq!(after("x1\nx2\ny\nx3\n", "", "g/x/normal dd"), "y\n");
    assert_eq!(after("x1\ny\nx2\n", "", "g/x/t$"), "x1\ny\nx2\nx1\nx2\n");
    assert_eq!(after("a\nb\nc\nd\n", "", "g/a\\|c/s/\\n/-/"), "a-b\nc-d\n");
    assert_eq!(after("x\ny\nx\n", "", "2,3g/x/d"), "x\ny\n");
}

#[test]
fn global_is_one_undo_step_and_moves_marks_with_the_lines() {
    let mut setup = Setup::new("x\na\nx\nb\n");
    setup.keys("Gma");
    run(&mut setup, "g/x/d");
    assert_eq!(setup.text(), "a\nb\n");
    setup.keys("gg'a");
    assert_eq!(setup.cursor(), (1, 0));
    setup.keys("u");
    assert_eq!(setup.text(), "x\na\nx\nb\n");
    // Its edits go to the daemon as one.
    let mut setup = Setup::new("x1\ny\nx2\n");
    setup
        .editor
        .ex("g/x/normal Aq", &mut setup.copy, &mut setup.shared, false)
        .unwrap();
    let edits = (setup.copy.take_outbox().into_iter())
        .filter(|message| matches!(message, ClientMessage::Edit { .. }))
        .count();
    assert_eq!(edits, 1);
    // Mistakes are said, and stop a macro.
    let mut setup = Setup::new("x\n");
    assert!(
        ex(&mut setup, "g/x/frob")
            .unwrap_err()
            .contains("frob isn't a command")
    );
    assert!(
        ex(&mut setup, "g/x/g/x/d")
            .unwrap_err()
            .contains("can't be done by a :g")
    );
    assert!(
        ex(&mut setup, "g/zzz/d")
            .unwrap_err()
            .contains("zzz isn't in")
    );
    assert!(
        ex(&mut setup, "v/x/d")
            .unwrap_err()
            .contains("every line has x")
    );
    assert!(
        ex(&mut setup, "g/x/")
            .unwrap_err()
            .contains("needs a command")
    );
    assert_eq!(setup.text(), "x\n");
}

#[test]
fn lines_are_deleted_yanked_moved_and_copied() {
    let lines = "1\n2\n3\n4\n5\n";
    assert_eq!(after(lines, "", "2,3m$"), "1\n4\n5\n2\n3\n");
    assert_eq!(after(lines, "G", "m0"), "5\n1\n2\n3\n4\n");
    assert_eq!(after(lines, "G", "m-2"), "1\n2\n3\n5\n4\n");
    assert_eq!(after(lines, "", "1t."), "1\n1\n2\n3\n4\n5\n");
    assert_eq!(after(lines, "G", "t0"), "5\n1\n2\n3\n4\n5\n");
    assert_eq!(after(lines, "", "1,2co$"), "1\n2\n3\n4\n5\n1\n2\n");
    assert_eq!(after(lines, "", "2d 2"), "1\n4\n5\n");
    assert_eq!(after(lines, "", "$d"), "1\n2\n3\n4\n");
    // A last line with no break after it.
    assert_eq!(after("1\n2\n3", "", "1m$"), "2\n3\n1");
    assert_eq!(after("1\n2\n3", "", "3m0"), "3\n1\n2");
    assert_eq!(after("1\n2\n3", "", "1t$"), "1\n2\n3\n1");
    let mut setup = Setup::new(lines);
    run(&mut setup, "2,3m$");
    assert_eq!(setup.cursor(), (4, 0));
    assert!(
        ex(&mut setup, "1,3m2")
            .unwrap_err()
            .contains("into themselves")
    );
    // Registers.
    let mut setup = Setup::new(lines);
    run(&mut setup, "2,3y a");
    assert_eq!(setup.held('a').as_deref(), Some("2\n3\n"));
    run(&mut setup, "4y A");
    assert_eq!(setup.held('a').as_deref(), Some("2\n3\n4\n"));
    run(&mut setup, "1d");
    assert_eq!(setup.held('1').as_deref(), Some("1\n"));
    assert_eq!(setup.text(), "2\n3\n4\n5\n");
}

/// Where `mark` is, as its line and column.
fn marked(setup: &Setup, mark: char) -> (u64, usize) {
    let at = (setup.shared.marks.get(mark, setup.copy.file()))
        .unwrap_or_else(|| panic!("mark {mark} isn't set"));
    (at.line, at.col)
}

/// What `gv` selects, as the lines and columns of its anchor and cursor.
fn reselected(setup: &mut Setup) -> ((u64, usize), (u64, usize)) {
    setup.keys("gv");
    let anchor = setup.editor.anchor.expect("a selection");
    let selected = ((anchor.line, anchor.col), setup.cursor());
    setup.keys("<Esc>");
    selected
}

#[test]
fn marks_on_lines_moved_go_with_them_and_gv_selects_them_again() {
    // `a` on the block, `c` on the line it jumps, `d` past them both.
    let mut setup = Setup::new("a\nb1\nc\nd\ne\n");
    setup.keys("jlmajmcjmdggVj<Esc>");
    run(&mut setup, "'<,'>m '>+1");
    assert_eq!(setup.text(), "c\na\nb1\nd\ne\n");
    assert_eq!(marked(&setup, 'a'), (2, 1));
    assert_eq!(marked(&setup, 'c'), (0, 0), "up by as many as were moved");
    assert_eq!(marked(&setup, 'd'), (3, 0));
    assert_eq!((marked(&setup, '<'), marked(&setup, '>')), ((1, 0), (2, 0)));
    assert_eq!(reselected(&mut setup), ((1, 0), (2, 0)));
    // Again, and so on down.
    run(&mut setup, "'<,'>m '>+1");
    assert_eq!(setup.text(), "c\nd\na\nb1\ne\n");
    assert_eq!(marked(&setup, 'a'), (3, 1));
    assert_eq!(marked(&setup, 'd'), (1, 0));
    assert_eq!(reselected(&mut setup), ((2, 0), (3, 0)));
    // And up.
    run(&mut setup, "'<,'>m '<-2");
    assert_eq!(setup.text(), "c\na\nb1\nd\ne\n");
    assert_eq!(marked(&setup, 'a'), (2, 1));
    assert_eq!(marked(&setup, 'd'), (3, 0));
    assert_eq!(reselected(&mut setup), ((1, 0), (2, 0)));
    // Undone, the move back carries them back.
    setup.keys("u");
    assert_eq!(setup.text(), "c\nd\na\nb1\ne\n");
    assert_eq!(marked(&setup, 'a'), (3, 1));
    assert_eq!(marked(&setup, 'd'), (1, 0));
    assert_eq!(reselected(&mut setup), ((2, 0), (3, 0)));
    // Ending on an empty line, whose line break is all that's selected of
    // it.
    let mut setup = Setup::new("a\n\nc\nd\n");
    setup.keys("Vj<Esc>");
    run(&mut setup, "'<,'>m '>+1");
    assert_eq!(setup.text(), "c\na\n\nd\n");
    assert_eq!(reselected(&mut setup), ((1, 0), (2, 0)));
    run(&mut setup, "'<,'>m '<-2");
    assert_eq!(setup.text(), "a\n\nc\nd\n");
    assert_eq!(reselected(&mut setup), ((0, 0), (1, 0)));
    // In its own mode, the cursor at the end it was.
    let mut setup = Setup::new("a\nbcd\nefg\nh\n");
    setup.keys("jjlvkh<Esc>");
    run(&mut setup, "'<,'>m0");
    assert_eq!(setup.text(), "bcd\nefg\na\nh\n");
    assert_eq!(reselected(&mut setup), ((1, 1), (0, 0)));
    setup.keys("gvd");
    assert_eq!(setup.text(), "g\na\nh\n");
}

#[test]
fn lines_moved_out_past_one_end_of_the_selection_cross_its_ends_over() {
    // Its last line up above it: it's from there to its first, the cursor
    // at the top.
    let mut setup = Setup::new("a\nb\nc\nd\n");
    setup.keys("jVj<Esc>");
    run(&mut setup, "3m0");
    assert_eq!(setup.text(), "c\na\nb\nd\n");
    assert_eq!((marked(&setup, '<'), marked(&setup, '>')), ((0, 0), (2, 0)));
    assert_eq!(reselected(&mut setup), ((2, 0), (0, 0)));
    // Its first line down below it.
    let mut setup = Setup::new("a\nb\nc\nd\n");
    setup.keys("jVj<Esc>");
    run(&mut setup, "2m$");
    assert_eq!(setup.text(), "a\nc\nd\nb\n");
    assert_eq!((marked(&setup, '<'), marked(&setup, '>')), ((1, 0), (3, 0)));
    assert_eq!(reselected(&mut setup), ((3, 0), (1, 0)));
}

#[test]
fn lines_moved_to_and_from_the_ends_take_their_marks() {
    // A block from the top to the end, and back.
    let mut setup = Setup::new("a\nb\nc\nd\n");
    setup.keys("majmbjmcVj<Esc>");
    run(&mut setup, "'<,'>m0");
    assert_eq!(setup.text(), "c\nd\na\nb\n");
    let marks = [
        marked(&setup, 'a'),
        marked(&setup, 'b'),
        marked(&setup, 'c'),
    ];
    assert_eq!(marks, [(2, 0), (3, 0), (0, 0)]);
    assert_eq!(reselected(&mut setup), ((0, 0), (1, 0)));
    run(&mut setup, "'<,'>m$");
    assert_eq!(setup.text(), "a\nb\nc\nd\n");
    let marks = [
        marked(&setup, 'a'),
        marked(&setup, 'b'),
        marked(&setup, 'c'),
    ];
    assert_eq!(marks, [(0, 0), (1, 0), (2, 0)]);
    assert_eq!(reselected(&mut setup), ((2, 0), (3, 0)));
    // With no line break after the last line, the break stays at the
    // end, and the marks with their lines.
    let mut setup = Setup::new("a\nbc\nde");
    setup.keys("majlmbjmd");
    run(&mut setup, "1m$");
    assert_eq!(setup.text(), "bc\nde\na");
    let marks = [
        marked(&setup, 'a'),
        marked(&setup, 'b'),
        marked(&setup, 'd'),
    ];
    assert_eq!(marks, [(2, 0), (0, 1), (1, 1)]);
    setup.keys("GVk<Esc>");
    run(&mut setup, "'<,'>m0");
    assert_eq!(setup.text(), "de\na\nbc");
    let marks = [
        marked(&setup, 'a'),
        marked(&setup, 'b'),
        marked(&setup, 'd'),
    ];
    assert_eq!(marks, [(1, 0), (2, 1), (0, 1)]);
    assert_eq!(reselected(&mut setup), ((1, 0), (0, 0)));
    setup.keys("u");
    assert_eq!(setup.text(), "bc\nde\na");
    let marks = [
        marked(&setup, 'a'),
        marked(&setup, 'b'),
        marked(&setup, 'd'),
    ];
    assert_eq!(marks, [(2, 0), (0, 1), (1, 1)]);
    // `:t` copies, and the marks stay on the lines copied.
    let mut setup = Setup::new("a\nb\n");
    setup.keys("majmb");
    run(&mut setup, "1,2t0");
    assert_eq!(setup.text(), "a\nb\na\nb\n");
    assert_eq!((marked(&setup, 'a'), marked(&setup, 'b')), ((2, 0), (3, 0)));
    run(&mut setup, "3,4t$");
    assert_eq!(setup.text(), "a\nb\na\nb\na\nb\n");
    assert_eq!((marked(&setup, 'a'), marked(&setup, 'b')), ((2, 0), (3, 0)));
}

#[test]
fn case_changes_go_through_the_operators() {
    let changed = |text: &str, keys: &str| {
        let mut setup = Setup::new(text);
        setup.keys(keys);
        (setup.text(), setup.cursor())
    };
    assert_eq!(changed("abcdef\n", "l3~"), ("aBCDef\n".into(), (0, 4)));
    assert_eq!(changed("abcdef\n", "4l5~"), ("abcdEF\n".into(), (0, 5)));
    assert_eq!(changed("abcdef\n", "2~."), ("ABCDef\n".into(), (0, 4)));
    assert_eq!(
        changed("foo barbaz qux\n", "6lgUiw"),
        ("foo BARBAZ qux\n".into(), (0, 4))
    );
    assert_eq!(
        changed("foo bar baz qux\n", "gUww."),
        ("FOO BAR baz qux\n".into(), (0, 4))
    );
    assert_eq!(changed("abcd\n", "2lgUU"), ("ABCD\n".into(), (0, 0)));
    assert_eq!(
        changed("abcd\nefgh\n", "2l2gUU"),
        ("ABCD\nEFGH\n".into(), (0, 2))
    );
    assert_eq!(changed("ab cd\n", "3lgUgU"), ("AB CD\n".into(), (0, 0)));
    assert_eq!(
        changed("ab\ncd\nef\ngh\n", "l3gUU"),
        ("AB\nCD\nEF\ngh\n".into(), (0, 1))
    );
    assert_eq!(changed("ABCD\n", "guu"), ("abcd\n".into(), (0, 0)));
    assert_eq!(changed("aBc\n", "g~~"), ("AbC\n".into(), (0, 0)));
    assert_eq!(changed("Hello\n", "g??"), ("Uryyb\n".into(), (0, 0)));
    assert_eq!(changed("abc def\n", "lvll~"), ("aBC def\n".into(), (0, 1)));
    assert_eq!(changed("ABC def\n", "vlu"), ("abC def\n".into(), (0, 0)));
    assert_eq!(
        changed("ab\ncd\nef\n", "lVjU"),
        ("AB\nCD\nef\n".into(), (0, 0))
    );
    assert_eq!(changed("ab cd\n", "vegU"), ("AB cd\n".into(), (0, 0)));
    // One character stays one, as in Vim: `ß` up to `ẞ`.
    assert_eq!(changed("straße\n", "gUU"), ("STRAẞE\n".into(), (0, 0)));
    assert_eq!(changed("ß x\n", "~~"), ("ẞ x\n".into(), (0, 2)));
    // `u` undoes it, in one step, and `.` with a count.
    let mut setup = Setup::new("one two three\n");
    setup.keys("g~iwu");
    assert_eq!(setup.text(), "one two three\n");
    setup.keys("gUww2.");
    assert_eq!(setup.text(), "ONE TWO THREE\n");
    // With `tildeop`, `~` takes a motion.
    let mut setup = Setup::new("abc def\n");
    setup.set(|options| options.tildeop = true);
    setup.keys("~w");
    assert_eq!(setup.text(), "ABC def\n");
}

#[test]
fn searches_read_vims_patterns() {
    let mut setup = Setup::new("abc a.c axc\n");
    setup.keys("/a\\.c<CR>");
    assert_eq!(setup.cursor(), (0, 4));
    setup.keys("/\\va(x|y)c<CR>");
    assert_eq!(setup.cursor(), (0, 8));
    setup.keys("N");
    assert_eq!(setup.cursor(), (0, 8));
    // `*` is `\<word\>`, as in Vim.
    let mut setup = Setup::new("cat concat cat\n");
    setup.keys("*");
    assert_eq!(setup.cursor(), (0, 11));
    // A pattern the engine can't do says why.
    let mut setup = Setup::new("aa\n");
    setup.keys("/\\(a\\)\\1<CR>");
    assert!(
        setup
            .editor
            .status
            .as_deref()
            .is_some_and(|status| status.contains("\\1"))
    );
}

#[test]
#[ignore = "a measurement, for release builds: cargo test --release -p clackatoo-gui -- --ignored hundred_thousand"]
fn substitute_on_a_hundred_thousand_lines() {
    let text = (0..100_000)
        .map(|line| format!("a line {line} with a few a's\n"))
        .collect::<Vec<_>>()
        .concat();
    let mut setup = Setup::new(&text);
    let started = std::time::Instant::now();
    setup
        .editor
        .ex("%s/a/b/g", &mut setup.copy, &mut setup.shared, false)
        .unwrap();
    let took = started.elapsed();
    let started = std::time::Instant::now();
    setup
        .editor
        .ex("g/9 with/d", &mut setup.copy, &mut setup.shared, false)
        .unwrap();
    let global = started.elapsed();
    let started = std::time::Instant::now();
    setup
        .editor
        .preview(Some("%s/b/c/g"), &setup.copy, &setup.shared);
    setup.editor.settle(&setup.copy);
    let preview = started.elapsed();
    println!(
        ":%s/a/b/g on 100k lines took {took:?}; :g/9 with/d {global:?}; a preview {preview:?}"
    );
    assert!(
        setup
            .copy
            .text()
            .to_string()
            .starts_with("b line 0 with b few b's\n")
    );
    if !cfg!(debug_assertions) {
        assert!(took.as_millis() < 100, "{took:?}");
    }
}

#[test]
#[ignore = "a measurement, for optimised builds: cargo test --profile quick -p clackatoo-gui -- --ignored global_substitute"]
fn global_substitute_on_two_megabytes() {
    // 40,000 lines of about 50 bytes, every other one with an x.
    let text = (0..40_000)
        .map(|line| {
            let mark = if line % 2 == 0 { 'x' } else { 'o' };
            format!("{mark} line {line:>8} of a file two megabytes long\n")
        })
        .collect::<Vec<_>>()
        .concat();
    let mut setup = Setup::new(&text);
    let started = std::time::Instant::now();
    let edit = setup
        .editor
        .ex("g/x/s/x/y/", &mut setup.copy, &mut setup.shared, false);
    let global = started.elapsed();
    edit.unwrap();
    assert!(setup.copy.text().to_string().starts_with("y line"));
    let mut keys = Vec::new();
    for typed in ["%s/l", "%s/li", "%s/lin", "%s/line/", "%s/line/L"] {
        let started = std::time::Instant::now();
        setup
            .editor
            .preview(Some(typed), &setup.copy, &setup.shared);
        setup.editor.settle(&setup.copy);
        keys.push(started.elapsed());
    }
    println!(
        ":g/x/s/x/y/ with 20k matches in {} bytes took {global:?}; preview keys {keys:?}",
        text.len()
    );
}
