//! Snippets a completion puts in, filled in place by place.

use super::{CompletionItem, Mode, Setup, completions};
use crate::render::HighlightKind;

/// Types `keys`, which leave the editor in insert mode after a word, then
/// accepts the snippet `text` for it, as the language server would
/// complete it.
fn complete(setup: &mut Setup, keys: &str, text: &str) {
    setup.server();
    setup.keys(keys);
    setup.tick();
    // Matching the word typed, whatever it is.
    let word: String = keys.chars().skip(1).collect();
    let item = CompletionItem {
        label: "s".into(),
        kind: 15,
        detail: String::new(),
        filter: Some(word),
        sort: None,
        preselect: false,
        start: None,
        text: text.into(),
        snippet: true,
        more: Vec::new(),
    };
    setup.answer(setup.last_completion().0, completions(vec![item], false));
    setup.keys("<C-y>");
}

/// The text, with the cursor shown as `|` and the text selected for
/// typing to replace between `[` and `]`.
fn shown(setup: &Setup) -> String {
    let mut text = setup.text();
    let at = setup.editor.cursor_byte(&setup.copy);
    let selected =
        (setup.editor.snippet.as_ref()).and_then(super::super::snippet::Session::selected);
    let mut marks = vec![(at, 0, '|')];
    if let Some((start, end)) = selected {
        marks.extend([(end, 1, ']'), (start, 2, '[')]);
    }
    marks.sort_unstable();
    for &(at, _, mark) in marks.iter().rev() {
        text.insert(at, mark);
    }
    text
}

/// Whether a snippet's being filled in.
fn filling(setup: &Setup) -> bool {
    setup.editor.snippet.is_some()
}

#[test]
fn tab_and_shift_tab_go_from_place_to_place_to_the_last() {
    let mut setup = Setup::new("\n");
    // As rust-analyzer completes a call.
    complete(&mut setup, "ica", "call(${1:a}, ${2:b})$0");
    assert_eq!(shown(&setup), "call([a|], b)\n");
    setup.keys("<Tab>");
    assert_eq!(shown(&setup), "call(a, [b|])\n");
    setup.keys("<S-Tab>");
    assert_eq!(shown(&setup), "call([a|], b)\n");
    // Shift+Tab at the first stays.
    setup.keys("<S-Tab>");
    assert_eq!(shown(&setup), "call([a|], b)\n");
    // The last, `$0`, finishes it, and Tab is Tab again.
    setup.keys("<Tab><Tab>");
    assert_eq!(shown(&setup), "call(a, b)|\n");
    assert!(!filling(&setup));
    setup.keys("<Tab>");
    assert_eq!(shown(&setup), "call(a, b)  |\n");
    // As is Shift+Tab.
    setup.keys("<S-Tab>");
    assert_eq!(shown(&setup), "call(a, b)      |\n");
}

#[test]
fn typing_replaces_a_places_text_and_backspace_deletes_it() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "ica", "call(${1:a}, ${2:b})$0");
    setup.keys("xy");
    assert_eq!(shown(&setup), "call(xy|, b)\n");
    setup.keys("<Tab><BS>");
    assert_eq!(shown(&setup), "call(xy, |)\n");
    setup.keys("z");
    assert_eq!(shown(&setup), "call(xy, z|)\n");
    // Back to a place typed in, it's selected afresh.
    setup.keys("<S-Tab>");
    assert_eq!(shown(&setup), "call([xy|], z)\n");
    // A key that doesn't type leaves it as it is.
    setup.keys("<Left>");
    assert_eq!(shown(&setup), "call(x|y, z)\n");
    assert!(filling(&setup));
    // All of it is one undo step, as typing is.
    setup.keys("<Esc>u");
    assert_eq!(setup.text(), "\n");
}

#[test]
fn an_empty_place_is_typed_in_and_what_comes_after_stays_after() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "if", "f($1$2)$0");
    assert_eq!(shown(&setup), "f(|)\n");
    setup.keys("ab<Tab>");
    assert_eq!(shown(&setup), "f(ab|)\n");
    setup.keys("c<S-Tab>");
    assert_eq!(shown(&setup), "f([ab|]c)\n");
}

#[test]
fn lines_after_the_first_take_its_indent_and_tabs_are_as_tab_puts_them() {
    // With spaces...
    let mut setup = Setup::new("    \n");
    complete(&mut setup, "Aif", "if ${1:cond} {\n\t$0\n}");
    assert_eq!(shown(&setup), "    if [cond|] {\n        \n    }\n");
    setup.keys("x<Tab>");
    assert_eq!(shown(&setup), "    if x {\n        |\n    }\n");
    // ...and with tabs, as the file is indented.
    let mut setup = Setup::new("\t\n");
    complete(&mut setup, "Aif", "if ${1:cond} {\n\t$0\n}");
    assert_eq!(shown(&setup), "\tif [cond|] {\n\t\t\n\t}\n");
}

#[test]
fn mirrors_copy_what_is_typed_as_it_is_typed() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "il", "let ${1:name} = 1; f($1, $1)");
    assert_eq!(shown(&setup), "let [name|] = 1; f(name, name)\n");
    setup.keys("x");
    assert_eq!(shown(&setup), "let x| = 1; f(x, x)\n");
    setup.keys("yz<BS>");
    assert_eq!(shown(&setup), "let xy| = 1; f(xy, xy)\n");
    // Adjacent to its place, a mirror still copies, not grows.
    let mut setup = Setup::new("\n");
    complete(&mut setup, "id", "${1:a}$1$0");
    setup.keys("bc");
    assert_eq!(shown(&setup), "bc|bc\n");
    setup.keys("<Tab>");
    assert_eq!(shown(&setup), "bcbc|\n");
    setup.keys("<Esc>u");
    assert_eq!(setup.text(), "\n", "one undo step");
}

#[test]
fn a_places_choices_are_picked_from_in_the_menu() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "ic", "${1|one,two,three|} ${2:end}");
    assert_eq!(shown(&setup), "[one|] end\n");
    assert_eq!(setup.menu().unwrap(), ["one", "two", "three"]);
    setup.keys("<C-n>");
    setup.tick();
    setup.keys("<C-y>");
    assert_eq!(shown(&setup), "two| end\n");
    assert_eq!(setup.menu(), None);
    setup.keys("<Tab>");
    assert_eq!(shown(&setup), "two [end|]\n");
    // Back to it, the menu shows again; typing closes it and replaces it.
    setup.keys("<S-Tab>");
    assert!(setup.menu().is_some());
    setup.keys("si");
    assert_eq!(shown(&setup), "si| end\n");
    assert_eq!(setup.menu(), None);
    // No server is asked about choices.
    assert!(
        (setup.queries.iter()).all(|query| !matches!(query.2, super::Query::Resolve { .. })),
        "{:?}",
        setup.queries
    );
}

#[test]
fn a_place_in_a_places_text_goes_once_that_is_typed_over() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "if", "f(${1:a, ${2:b}}) $3");
    assert_eq!(shown(&setup), "f([a, b|]) \n");
    // Left as it is, it's gone to next...
    setup.keys("<Tab>");
    assert_eq!(shown(&setup), "f(a, [b|]) \n");
    // ...but typed over, it's gone.
    setup.keys("<S-Tab>x<Tab>");
    assert_eq!(shown(&setup), "f(x) |\n");
}

#[test]
fn others_edits_move_the_places() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "ica", "call(${1:a}, ${2:b})$0");
    // An agent types before it, and in its second place.
    let agents = setup.daemon.agent(0, "zz ");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(agents);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    assert_eq!(shown(&setup), "zz call([a|], b)\n");
    let agents = setup.daemon.agent("zz call(a, b".len(), "!");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(agents);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    setup.keys("<Tab>");
    assert_eq!(shown(&setup), "zz call(a, [b|]!)\n", "not at its edge");
    setup.keys("q<Tab>");
    assert_eq!(shown(&setup), "zz call(a, q!)|\n");
}

#[test]
fn leaving_insert_mode_or_the_snippet_finishes_it() {
    let mut setup = Setup::new("\nnext\n");
    complete(&mut setup, "ica", "call(${1:a}, ${2:b})$0");
    setup.keys("<Esc>");
    assert!(!filling(&setup));
    assert_eq!(setup.editor.mode, Mode::Normal);
    setup.keys("A<Tab>");
    assert_eq!(setup.text(), "call(a, b)  \nnext\n", "Tab is Tab again");

    let mut setup = Setup::new("\nnext\n");
    complete(&mut setup, "ica", "call(${1:a}, ${2:b})$0");
    setup.keys("<Down>");
    assert!(!filling(&setup), "the cursor left it");
    setup.keys("<Up><End><Tab>");
    assert_eq!(setup.text(), "call(a, b)  \nnext\n");
}

#[test]
fn a_snippet_with_nowhere_to_type_but_its_end_is_put_in_as_it_is() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "if", "f()$0;");
    assert_eq!(shown(&setup), "f()|;\n");
    assert!(!filling(&setup));
}

#[test]
fn an_unknown_variable_is_its_name_to_type_over() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "id", "// $TM_FILENAME_BASE: $UNKNOWN_THING");
    assert_eq!(shown(&setup), "// a: [UNKNOWN_THING|]\n");
}

#[test]
fn the_place_gone_to_is_lit_as_a_selection() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "ica", "call(${1:abc})$0");
    // Right of the line numbers' four cells.
    assert_eq!(setup.highlighted(HighlightKind::Selection), [(0, 9..12)]);
    setup.keys("x");
    assert_eq!(setup.highlighted(HighlightKind::Selection), []);
}

#[test]
fn dot_types_what_the_snippet_left() {
    let mut setup = Setup::new("\n\n");
    complete(&mut setup, "ica", "call(${1:a}, ${2:b})$0");
    setup.keys("x<Tab>y<Esc>");
    assert_eq!(setup.text(), "call(x, y)\n\n");
    setup.keys("j0.");
    assert_eq!(setup.text(), "call(x, y)\ncall(x, y)\n");
}

#[test]
fn text_pasted_replaces_a_places_text_and_is_mirrored() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "il", "let ${1:name} = $1;");
    setup.editor.paste("pasted", &mut setup.copy);
    setup.serve();
    assert_eq!(shown(&setup), "let pasted| = pasted;\n");
}

#[test]
fn a_new_lines_indent_put_right_keeps_the_cursor_in_its_place() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "ica", "call(${1:a}) $1$0");
    setup.keys("|x| {<CR>");
    assert_eq!(shown(&setup), "call(|x| {\n|) |x| {\n\n");
    // The grammar's indent, when it comes, goes before the cursor, in its
    // place, so what's typed after is still copied to its mirror.
    let (asked, at, _) = *setup.indents.last().expect("asked how to indent");
    let line = clackatoo_protocol::LineIndent {
        line: at,
        width: Some(4),
    };
    setup.answer(asked, super::Answer::Indents(vec![line]));
    assert!(shown(&setup).starts_with("call(|x| {\n    |)"));
    setup.keys("y");
    assert_eq!(shown(&setup), "call(|x| {\n    y|) |x| {\n    y\n");
}

#[test]
fn dot_indents_a_snippets_lines_as_the_line_it_types_on() {
    let mut setup = Setup::new("    \n\n");
    complete(&mut setup, "Aif", "if ${1:x} {\n\t$0\n}");
    setup.keys("a<Tab>b<Esc>");
    assert_eq!(setup.text(), "    if a {\n        b\n    }\n\n");
    setup.keys("jj.");
    assert_eq!(
        setup.text(),
        "    if a {\n        b\n    }\nif a {\n    b\n}\n"
    );
}

#[test]
fn with_completetab_tab_goes_on_from_a_place_with_choices() {
    let mut setup = Setup::new("\n");
    setup.set(|options| options.completetab = true);
    complete(&mut setup, "ic", "${1|one,two|} ${2:end}");
    assert!(setup.menu().is_some());
    setup.keys("<Tab>");
    assert_eq!(shown(&setup), "one [end|]\n");
}

#[test]
fn the_mirrors_of_a_place_typed_in_through_one_in_it_follow() {
    let mut setup = Setup::new("\n");
    complete(&mut setup, "if", "${1:f(${2:x})} = $1; $2");
    assert_eq!(shown(&setup), "[f(x)|] = f(x); x\n");
    setup.keys("<Tab>yz");
    assert_eq!(shown(&setup), "f(yz|) = f(yz); yz\n");
    setup.keys("<Esc>u");
    assert_eq!(setup.text(), "\n", "one undo step");
}
