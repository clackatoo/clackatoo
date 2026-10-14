//! Words as a buffer's language reads them: with the characters its pack
//! adds to letters, digits and `_`, such as CSS's `-`, in moving by words,
//! picking them out, `*` and completion; and as plain text without.

use clackatoo_protocol::{BufferUpdate, CompletionItem};

use super::{Setup, completions, item};
use crate::editor::pattern::TOO_COMPLEX;
use crate::motion::Words;

/// A setup editing `text`, whose language puts `word` in words.
fn with_words(text: &str, word: &str) -> Setup {
    let mut setup = Setup::new(text);
    let word = word.to_owned();
    setup.copy.update(BufferUpdate::Language { word });
    setup
}

/// The columns the cursor goes to on the first line with each of `keys`,
/// typed one after another from its start.
fn stops(setup: &mut Setup, keys: &[&str]) -> Vec<usize> {
    setup.keys("gg0");
    (keys.iter())
        .map(|keys| {
            setup.keys(keys);
            setup.cursor().1
        })
        .collect()
}

#[test]
fn words_have_what_the_language_puts_in_them() {
    let text = "a { margin-top: 0; }\n";
    let mut setup = with_words(text, "-");
    assert_eq!(setup.copy.words(), Words::new("-"));
    // `w`, `e` and `b` take `margin-top` whole, and `:` is still
    // punctuation.
    assert_eq!(stops(&mut setup, &["w", "w", "w", "w"]), [2, 4, 14, 16]);
    assert_eq!(stops(&mut setup, &["ww", "e", "e"]), [4, 13, 14]);
    assert_eq!(
        stops(&mut setup, &["$", "b", "b", "b", "b"]),
        [19, 17, 16, 14, 4]
    );
    // `W` and its kin are as they always are.
    assert_eq!(stops(&mut setup, &["W", "W", "W"]), [2, 4, 16]);
    // So are the operators that go with them, and the objects.
    setup.keys("gg04ldiw");
    assert_eq!(setup.text(), "a { : 0; }\n");
    // With no blanks after it, `aw` takes those before.
    setup.keys("u07ldaw");
    assert_eq!(setup.text(), "a {: 0; }\n");
    setup.keys("u04lcwpadding<Esc>");
    assert_eq!(setup.text(), "a { padding: 0; }\n");
    // A fresh snapshot is read as plain text, until the daemon says again.
    let snapshot = setup.daemon.snapshot();
    setup.copy.update(snapshot);
    assert_eq!(setup.copy.words(), Words::NONE);

    // Plain text, as before: `-` is punctuation.
    let mut setup = Setup::new(text);
    assert_eq!(stops(&mut setup, &["w", "w", "w", "w"]), [2, 4, 10, 11]);
    setup.keys("gg04ldiw");
    assert_eq!(setup.text(), "a { -top: 0; }\n");
}

/// Does `line`, typed after `:`, as the desk does, and serves the daemon.
fn ex(setup: &mut Setup, line: &str) -> Result<(), String> {
    let done = (setup.editor)
        .ex(line, &mut setup.copy, &mut setup.shared, false)
        .map(|_| ());
    setup.serve();
    done
}

#[test]
fn a_search_too_complex_to_finish_says_so_and_changes_nothing() {
    // A match on the first line, and on the second, one the search gives
    // up on, though the regex crate's `b` brings it to look.
    let text = "x-b\n".to_owned() + &"a".repeat(40) + "b-\n";
    let mut setup = with_words(&text, "-");
    // From the second line, `/` doesn't go round to the first's, as the
    // search past the cursor that gave up may have missed one; nor do `n`
    // and `N`.
    for keys in ["/\\(a*\\)*b\\><CR>", "n", "N"] {
        setup.keys("gg0j");
        setup.editor.status = None;
        setup.keys(keys);
        assert_eq!(setup.editor.status.as_deref(), Some(TOO_COMPLEX), "{keys}");
        assert_eq!(setup.cursor(), (1, 0), "{keys}");
    }
    // `:s`, with `c` asking about each, or `n` counting, and `:g`, change
    // nothing, ask nothing and count nothing, and say why.
    for line in [
        "s/\\(a*\\)*b\\>/x/",
        "s/\\(a*\\)*b\\>/x/c",
        "s/\\(a*\\)*b\\>/x/n",
        "s/\\(a*\\)*b\\>/x/e",
        "g/\\(a*\\)*b\\>/d",
    ] {
        assert_eq!(ex(&mut setup, line), Err(TOO_COMPLEX.to_owned()), "{line}");
        assert!(setup.editor.prompt.is_none(), "{line}");
        assert_eq!(setup.text(), text, "{line}");
    }
}

#[test]
fn ctrl_w_deletes_a_word_as_the_language_reads_it() {
    let mut setup = with_words("\n", "-");
    setup.keys("Oa margin-top<C-w><Esc>");
    assert_eq!(setup.text(), "a \n\n");
    // In plain text, back to the `-`.
    let mut setup = Setup::new("\n");
    setup.keys("Oa margin-top<C-w><Esc>");
    assert_eq!(setup.text(), "a margin-\n\n");
}

#[test]
fn a_word_can_start_and_end_with_what_the_language_puts_in_it() {
    let text = "--main-color: red;\nb { color: var(--main-color); }\n";
    let mut setup = with_words(text, "-");
    setup.keys("gg0diw");
    assert_eq!(setup.text(), ": red;\nb { color: var(--main-color); }\n");
    setup.keys("u");
    // `*` finds the whole name, starting with its `--`, and only it.
    setup.keys("gg0*");
    assert_eq!(setup.cursor(), (1, "b { color: var(".len()));
    setup.keys("n");
    assert_eq!(setup.cursor(), (0, 0), "round the end");
}

#[test]
fn star_finds_whole_words_as_the_language_reads_them() {
    let text = "libcap\nlibcap-tools\nodd.libcap-tools\nlibcap-tools-extra\n";
    let mut setup = with_words(text, "-");
    // `libcap` is in `libcap-tools`, but isn't a word of it.
    setup.keys("gg*");
    assert_eq!(setup.cursor(), (0, 0), "only itself");
    setup.keys("j*");
    assert_eq!(setup.cursor(), (2, 4));
    setup.keys("n");
    assert_eq!(setup.cursor(), (1, 0), "not in libcap-tools-extra");
    setup.keys("#");
    assert_eq!(setup.cursor(), (2, 4));
    // A pattern typed reads `\<`, `\>` and `\k` so too.
    setup.keys("gg/\\<lt>tools\\><CR>");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some("\\<tools\\> isn't in the file")
    );
    setup.keys("gg/\\<lt>\\k\\+-extra\\><CR>");
    assert_eq!(setup.cursor(), (3, 0));

    // In plain text, `*` on `libcap` finds it in `libcap-tools`.
    let mut setup = Setup::new(text);
    setup.keys("gg*");
    assert_eq!(setup.cursor(), (1, 0));
    setup.keys("*");
    assert_eq!(setup.cursor(), (2, 4), "and in odd.libcap-tools");
}

/// An item labelled `label`, from a server that doesn't say where the word
/// it replaces starts.
fn unplaced(label: &str) -> CompletionItem {
    CompletionItem {
        start: None,
        ..item(label, 0)
    }
}

/// The text after typing `typed` on a new first line of `text`, asking
/// what could finish it, and accepting `label`, from a server that doesn't
/// say where the word starts.
fn completed(setup: &mut Setup, typed: &str, label: &str) -> String {
    setup.server();
    setup.keys(&format!("O{typed}<C-Space>"));
    setup.tick();
    let (asked, ..) = setup.last_completion();
    setup.answer(asked, completions(vec![unplaced(label)], false));
    setup.keys("<C-y><Esc>");
    setup.text()
}

#[test]
fn completing_a_name_with_dashes_puts_it_in_once() {
    // As the language reads it, `libcap-t` is the word: what's shown
    // matches all of it, and goes in place of all of it.
    let mut setup = with_words("\n", "-");
    setup.server();
    setup.keys("Oodd.libcap-t<C-Space>");
    let (asked, ..) = setup.last_completion();
    let items = vec![unplaced("libcap-tools"), unplaced("tar")];
    setup.answer(asked, completions(items, false));
    assert_eq!(setup.menu().unwrap(), ["libcap-tools"]);
    // Typing on narrows it, as a word's character.
    setup.keys("o");
    assert_eq!(setup.menu().unwrap(), ["libcap-tools"]);
    setup.keys("<C-y><Esc>");
    assert_eq!(setup.text(), "odd.libcap-tools\n\n");

    // In plain text, the word is the `t`, which `tar` matches too.
    let mut setup = Setup::new("\n");
    setup.server();
    setup.keys("Oodd.libcap-t<C-Space>");
    let (asked, ..) = setup.last_completion();
    let items = vec![unplaced("libcap-tools"), unplaced("tar")];
    setup.answer(asked, completions(items, false));
    assert_eq!(setup.menu().unwrap(), ["tar", "libcap-tools"]);
}

#[test]
fn without_words_a_name_is_widened_back_over_what_it_starts_with() {
    let mut setup = Setup::new("\n");
    assert_eq!(
        completed(&mut setup, "odd.libcap-t", "libcap-tools"),
        "odd.libcap-tools\n\n"
    );
    // Asked right after the `-`, with nothing of the word typed.
    let mut setup = Setup::new("\n");
    assert_eq!(
        completed(&mut setup, "odd.libcap-", "libcap-tools"),
        "odd.libcap-tools\n\n"
    );
    // What the label doesn't start with is left alone...
    let mut setup = Setup::new("\n");
    assert_eq!(
        completed(&mut setup, "odd.lib-t", "libcap-tools"),
        "odd.lib-libcap-tools\n\n"
    );
    // ...and nothing's taken past a blank, though the label starts with
    // what's before it.
    let mut setup = Setup::new("\n");
    assert_eq!(completed(&mut setup, "a b-c", "a b-cd"), "a b-a b-cd\n\n");
}

#[test]
fn what_was_there_before_the_word_is_kept_not_put_in_again() {
    // `libcap-` was there before the insert; only `tools` goes in, in
    // place of the `t`, so `.` elsewhere types `t` and makes it `tools`,
    // never taking out what's before the cursor there.
    let mut setup = Setup::new("odd.libcap-\nfoo bar\n");
    setup.server();
    setup.keys("At<C-Space>");
    let (asked, ..) = setup.last_completion();
    setup.answer(asked, completions(vec![unplaced("libcap-tools")], false));
    setup.keys("<C-y><Esc>");
    assert_eq!(setup.text(), "odd.libcap-tools\nfoo bar\n");
    setup.keys("j.");
    assert_eq!(setup.text(), "odd.libcap-tools\nfoo bartools\n");
    // Undone in one step, back to what was there.
    setup.keys("uu");
    assert_eq!(setup.text(), "odd.libcap-\nfoo bar\n");
}
