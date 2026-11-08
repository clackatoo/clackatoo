//! `r` and a character: replacing as many as the count, failing when the
//! line hasn't that many. Each case was checked against Neovim 0.11,
//! typed into a fresh Neovim each.

use super::Setup;

#[test]
fn r_with_a_count_past_the_lines_end_changes_nothing_and_fails() {
    let mut setup = Setup::new("abc\n");
    setup.keys("l5rx");
    assert_eq!(setup.text(), "abc\n");
    assert_eq!(setup.cursor(), (0, 1));
    assert!(setup.editor.failed, "a macro stops there, as Vim's beep");
    // As many as there are goes.
    let mut setup = Setup::new("abc\n");
    setup.keys("l2rx");
    assert_eq!(setup.text(), "axx\n");
    assert!(!setup.editor.failed);
}

/// The text and cursor after typing `keys` into `text`.
fn after(text: &str, keys: &str) -> (String, (u64, usize)) {
    let mut setup = Setup::new(text);
    setup.keys(keys);
    (setup.text(), setup.cursor())
}

#[test]
fn r_and_enter_breaks_the_line_in_place_of_as_many_characters() {
    assert_eq!(after("abc def\n", "3lr<CR>"), ("abc\ndef\n".into(), (1, 0)));
    assert_eq!(after("abc def\n", "2l3r<CR>"), ("ab\nef\n".into(), (1, 0)));
    // The new line takes the indent, and the blanks after go.
    assert_eq!(
        after("  abc def\n", "5lr<CR>"),
        ("  abc\n  def\n".into(), (1, 1))
    );
    assert_eq!(
        after("abc   def\n", "4lr<CR>"),
        ("abc \ndef\n".into(), (1, 0))
    );
    // Nothing after, no indent.
    assert_eq!(after("  abc\n", "4lr<CR>"), ("  ab\n\n".into(), (1, 0)));
    assert_eq!(after("abc  \n", "l3r<CR>"), ("a\n\n".into(), (1, 0)));
}

#[test]
fn r_and_enter_past_the_end_fails_and_undoes_and_repeats_as_one() {
    let mut setup = Setup::new("abc\n");
    setup.keys("9r<CR>");
    assert_eq!(setup.text(), "abc\n");
    assert!(setup.editor.failed);
    let mut setup = Setup::new("abcdef\nghijkl\n");
    setup.keys("l2r<CR>j.");
    assert_eq!(setup.text(), "a\ndef\n\nijkl\n");
    assert_eq!(setup.cursor(), (3, 0));
    setup.keys("uu");
    assert_eq!(setup.text(), "abcdef\nghijkl\n");
}
