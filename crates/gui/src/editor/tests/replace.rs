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
