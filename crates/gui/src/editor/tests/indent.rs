//! Indenting as the grammar says: a new line's indent put right when the
//! answer comes, whatever was typed meanwhile, and `=`.

use clackatoo_protocol::{Answer, Construct, LineIndent, Piece, Query};

use super::{Mode, Setup};

/// The last question how lines should be indented: its number, the byte
/// it asks about, and how many lines, as the text is now.
fn last_indent(setup: &Setup) -> (u64, usize, usize) {
    let (id, at, query) = *setup.indents.last().expect("asked how to indent");
    let Query::Indent {
        end,
        shiftwidth,
        tabstop,
    } = query
    else {
        unreachable!("only indents are kept there");
    };
    assert_eq!((shiftwidth, tabstop), (4, 4), "as the options say");
    let text = setup.copy.text();
    (id, at, text.byte_to_line(end) - text.byte_to_line(at) + 1)
}

/// The grammar's answer: each line, by the byte it starts at, and its
/// width.
fn indents(lines: &[(usize, Option<u32>)]) -> Answer {
    let indents = (lines.iter())
        .map(|&(line, width)| LineIndent { line, width })
        .collect();
    Answer::Indents(indents)
}

#[test]
fn a_new_line_takes_the_indent_before_at_once_and_the_grammars_when_it_comes() {
    let mut setup = Setup::new("fn a() {\n}\n");
    setup.keys("A<CR>");
    assert_eq!(
        setup.text(),
        "fn a() {\n\n}\n",
        "as the line before, at once"
    );
    let (asked, at, lines) = last_indent(&setup);
    assert_eq!((at, lines), (9, 1));
    setup.answer(asked, indents(&[(9, Some(4))]));
    assert_eq!(setup.text(), "fn a() {\n    \n}\n");
    assert_eq!(setup.cursor(), (1, 4));
    assert_eq!(setup.editor.mode, Mode::Insert);
    setup.keys("x<Esc>");
    assert_eq!(setup.text(), "fn a() {\n    x\n}\n");
    // One undo step, with what was typed.
    setup.keys("u");
    assert_eq!(setup.text(), "fn a() {\n}\n");
}

#[test]
fn what_was_typed_while_the_answer_came_stays_after_the_indent() {
    let mut setup = Setup::new("    if a {\n    }\n");
    setup.keys("A<CR>let x");
    assert_eq!(setup.text(), "    if a {\n    let x\n    }\n");
    let (asked, at, _) = last_indent(&setup);
    setup.answer(asked, indents(&[(at, Some(8))]));
    assert_eq!(setup.text(), "    if a {\n        let x\n    }\n");
    assert_eq!(setup.cursor(), (1, 13), "still after what was typed");
    // And typing goes on there.
    setup.keys(";");
    assert_eq!(setup.text(), "    if a {\n        let x;\n    }\n");
}

#[test]
fn an_answer_after_esc_is_part_of_the_change_that_made_the_line() {
    let mut setup = Setup::new("{\n}\n");
    setup.keys("ox<Esc>");
    let (asked, at, _) = last_indent(&setup);
    assert_eq!(at, 2);
    setup.answer(asked, indents(&[(at, Some(4))]));
    assert_eq!(setup.text(), "{\n    x\n}\n");
    assert_eq!(setup.cursor(), (1, 4), "still on the x");
    setup.keys("u");
    assert_eq!(setup.text(), "{\n}\n");
    // `O` asks too, about the line it opens.
    setup.keys("jOy<Esc>");
    let (_, at, _) = last_indent(&setup);
    assert_eq!((setup.text(), at), ("{\ny\n}\n".into(), 2));
}

#[test]
fn an_indent_typed_in_or_not_a_lines_any_more_is_left_as_it_is() {
    let mut setup = Setup::new("    a\n");
    setup.keys("A<CR><BS>b");
    assert_eq!(setup.text(), "    a\n   b\n");
    let (asked, at, _) = last_indent(&setup);
    setup.answer(asked, indents(&[(at, Some(8))]));
    assert_eq!(setup.text(), "    a\n   b\n", "the user's");
    // One saying to keep it, or one that failed.
    for answer in [
        indents(&[(at, None)]),
        indents(&[]),
        Answer::Nothing,
        Answer::Failed("the text changed meanwhile".into()),
    ] {
        setup.keys("<CR>");
        setup.answer(last_indent(&setup).0, answer);
    }
    assert_eq!(setup.text(), "    a\n   b\n   \n   \n   \n   \n");
}

/// Another author's edit to the daemon's text, replacing `removed` at byte
/// `at` with `text`, as the window hears of it.
fn agent(setup: &mut Setup, at: usize, removed: &str, text: &str) {
    let update = setup.daemon.agent_replace(at, removed, text);
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(update);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
}

#[test]
fn a_line_taken_out_meanwhile_isnt_put_right_and_neither_is_the_next() {
    let mut setup = Setup::new("    }\n");
    setup.keys("O<Esc>");
    assert_eq!(setup.text(), "    \n    }\n");
    let (asked, ..) = last_indent(&setup);
    // Another window's `dd` takes the new line out: the `}` after it
    // keeps its indent.
    agent(&mut setup, 0, "    \n", "");
    setup.answer(asked, indents(&[(0, Some(8))]));
    assert_eq!(setup.text(), "    }\n");
    // So does one joined to the line before.
    let mut setup = Setup::new("a\n");
    setup.keys("o<Esc>");
    let (asked, ..) = last_indent(&setup);
    agent(&mut setup, 1, "\n", "");
    setup.answer(asked, indents(&[(1, Some(8))]));
    assert_eq!(setup.text(), "a\n");
}

#[test]
fn a_line_moved_meanwhile_is_put_right_where_it_is() {
    let mut setup = Setup::new("{\n}\n");
    setup.keys("A<CR>x");
    // Another window puts lines in above it.
    agent(&mut setup, 0, "", "// one\n// two\n");
    let (asked, ..) = last_indent(&setup);
    // The answer's place doesn't matter: the editor knows where it is.
    setup.answer(asked, indents(&[(2, Some(4))]));
    assert_eq!(setup.text(), "// one\n// two\n{\n    x\n}\n");
}

#[test]
fn a_change_made_before_the_answer_comes_keeps_it_out() {
    let mut setup = Setup::new("{\n}\n");
    setup.keys("ox<Esc>x");
    assert_eq!(setup.text(), "{\n\n}\n");
    let (asked, at, _) = last_indent(&setup);
    // Joining `x`'s undo step would undo the two at once; one of its own
    // would undo an indent nobody typed.
    setup.answer(asked, indents(&[(at, Some(4))]));
    assert_eq!(setup.text(), "{\n\n}\n");
    setup.keys("u");
    assert_eq!(setup.text(), "{\nx\n}\n", "the x's undone alone");
    // Nor after an undo, which would lose what redo brings back.
    setup.keys("A<CR><Esc>u");
    let (asked, at, _) = last_indent(&setup);
    setup.answer(asked, indents(&[(at, Some(4))]));
    assert_eq!(setup.text(), "{\nx\n}\n");
    setup.keys("<C-r>");
    assert_eq!(setup.text(), "{\nx\n\n}\n");
}

#[test]
fn a_macro_waits_for_its_new_lines_indents_and_the_oldest_waiting_go() {
    let mut setup = Setup::new("{\n}\n");
    let since = crate::editor::copy::queries_asked();
    setup.keys("o");
    assert!(setup.editor.waiting_for_answer(&setup.copy, since));
    let (asked, at, _) = last_indent(&setup);
    setup.answer(asked, indents(&[(at, Some(4))]));
    assert!(!setup.editor.waiting_for_answer(&setup.copy, since));
    // Asked so many times with no answer, the first are given up on.
    for _ in 0..40 {
        setup.keys("<CR>");
    }
    let (first, ..) = setup.indents[1];
    let (last, at, _) = last_indent(&setup);
    setup.answer(first, indents(&[(2, Some(8))]));
    assert_eq!(
        setup.copy.text().line(2).to_string(),
        "    \n",
        "given up on"
    );
    setup.answer(last, indents(&[(at, Some(0))]));
    assert_eq!(setup.copy.text().line(41).to_string(), "\n");
}

#[test]
fn another_question_about_the_line_stands_over_the_first() {
    let mut setup = Setup::new("    if a {\n        b\n");
    setup.keys("jo}");
    assert_eq!(setup.text(), "    if a {\n        b\n        }\n");
    assert_eq!(setup.indents.len(), 2, "the o's and the }}'s");
    let (opened, ..) = setup.indents[0];
    let (closed, at, _) = last_indent(&setup);
    assert_eq!(at, 21);
    // The o's answer, about the line as it was, isn't the one that
    // stands.
    setup.answer(opened, indents(&[(at, Some(12))]));
    assert_eq!(setup.text(), "    if a {\n        b\n        }\n");
    setup.answer(closed, indents(&[(at, Some(4))]));
    assert_eq!(setup.text(), "    if a {\n        b\n    }\n");
    assert_eq!(setup.cursor(), (2, 5));
}

#[test]
fn a_closing_bracket_or_else_typed_first_on_a_line_asks_again() {
    let mut setup = Setup::new("x\n");
    let asked = |setup: &mut Setup, keys: &str| {
        let before = setup.indents.len();
        setup.keys(keys);
        setup.indents.len() - before
    };
    assert_eq!(asked(&mut setup, "o  ]"), 2);
    assert_eq!(asked(&mut setup, "<CR>  else:"), 2);
    // Not one after other text, nor `:` after another word.
    assert_eq!(asked(&mut setup, "<CR>a)"), 1);
    assert_eq!(asked(&mut setup, "<CR>a:"), 1);
    // Nor with smartindent off.
    setup.keys("<Esc>");
    setup.set(|options| options.smartindent = false);
    assert_eq!(asked(&mut setup, "o}<Esc>O<CR>"), 0);
}

const BLOCK: &str = "a {\nb\n\n  c\n}\n";

#[test]
fn reindenting_waits_for_the_grammar_then_indents_each_line_at_once() {
    let mut setup = Setup::new(BLOCK);
    setup.keys("jgg=Gx");
    let (asked, at, lines) = last_indent(&setup);
    assert_eq!((at, lines), (0, 5));
    assert_eq!(setup.text(), BLOCK, "not until it answers");
    // A blank line stays empty, and one the rules say to keep keeps its
    // indent.
    let answer = [
        (0, Some(0)),
        (4, Some(4)),
        (6, Some(4)),
        (7, None),
        (11, Some(0)),
    ];
    setup.answer(asked, indents(&answer));
    // The x was held, and deletes the first line's first character once
    // `=` has put the cursor there.
    assert_eq!(setup.text(), " {\n    b\n\n  c\n}\n");
    setup.keys("u");
    assert_eq!(setup.text(), "a {\n    b\n\n  c\n}\n");
    setup.keys("u");
    assert_eq!(setup.text(), BLOCK, "in one undo step");
}

#[test]
fn reindenting_lines_with_a_count_a_selection_and_again() {
    let mut setup = Setup::new(BLOCK);
    setup.keys("j2==");
    let (asked, at, lines) = last_indent(&setup);
    assert_eq!((at, lines), (4, 2));
    setup.answer(asked, indents(&[(4, Some(4)), (6, Some(4))]));
    assert_eq!(setup.text(), "a {\n    b\n\n  c\n}\n");
    assert_eq!(setup.cursor(), (1, 4));
    // `.` asks again, about the lines from the cursor's.
    setup.keys("jj.");
    let (asked, at, lines) = last_indent(&setup);
    assert_eq!((at, lines), (11, 2));
    setup.answer(asked, indents(&[(11, Some(4)), (15, Some(0))]));
    assert_eq!(setup.text(), "a {\n    b\n\n    c\n}\n");
    // In visual mode, the lines selected, and the selection goes.
    setup.keys("ggVj=");
    let (asked, at, lines) = last_indent(&setup);
    assert_eq!((at, lines), (0, 2));
    setup.answer(asked, indents(&[(0, Some(2)), (4, Some(2))]));
    assert_eq!(setup.text(), "  a {\n  b\n\n    c\n}\n");
    assert_eq!(setup.editor.mode, Mode::Normal);
}

#[test]
fn reindenting_a_function_asks_where_it_is_first() {
    let text = "fn a() {\nx;\n}\n";
    let mut setup = Setup::new(text);
    setup.keys("j=af");
    let (asked, _, query) = setup.last_query();
    assert_eq!(query, Query::Pieces(Construct::Function));
    let piece = Piece {
        start: 0,
        end: text.len() - 1,
        inner: Some((9, 11)),
    };
    setup.answer(asked, Answer::Pieces(vec![piece]));
    let (asked, at, lines) = last_indent(&setup);
    assert_eq!((at, lines), (0, 3));
    setup.answer(asked, indents(&[(0, Some(0)), (9, Some(4)), (12, Some(0))]));
    assert_eq!(setup.text(), "fn a() {\n    x;\n}\n");
    assert_eq!(setup.cursor(), (0, 0));
    assert_eq!(setup.editor.status.as_deref(), Some("3 lines indented"));
}

#[test]
fn reindenting_without_a_grammar_or_rules_says_so() {
    let mut setup = Setup::new(BLOCK);
    setup.keys("==");
    setup.answer(last_indent(&setup).0, Answer::Nothing);
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("no grammar for this file's language, to indent by")
    );
    // The daemon says why it has no rules.
    let why = "no rules for indenting: run `clackatoo language install rust` to get them";
    setup.keys("==");
    setup.answer(last_indent(&setup).0, Answer::Failed(why.into()));
    assert_eq!(setup.editor.status.as_deref(), Some(why));
    setup.keys("==");
    setup.answer(last_indent(&setup).0, indents(&[]));
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("the lines were taken out meanwhile: nothing was done")
    );
    assert_eq!(setup.text(), BLOCK);
}

#[test]
#[ignore = "a timing, for an optimised build: cargo test --profile quick -- --ignored"]
fn reindenting_a_big_file_takes_a_frame_or_so() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/editor/mod.rs");
    let text = std::fs::read_to_string(path).unwrap();
    let mut setup = Setup::new(&text);
    setup.keys("gg=G");
    let (asked, _, lines) = last_indent(&setup);
    // Every line a step further in than it is.
    let rope = setup.copy.text().clone();
    let answer: Vec<(usize, Option<u32>)> = (0..lines)
        .map(|line| {
            let start = rope.line_to_byte(line);
            let indent = rope.line(line).chars().take_while(|ch| *ch == ' ').count();
            (start, Some(u32::try_from(indent).unwrap() + 4))
        })
        .collect();
    let took = std::time::Instant::now();
    setup.answer(asked, indents(&answer));
    let took = took.elapsed();
    eprintln!("the window's gg=G of {lines} lines, with the test's daemon: {took:?}");
    assert!(setup.text().starts_with("    //!"));
}
