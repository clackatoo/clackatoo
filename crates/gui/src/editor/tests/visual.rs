//! What visual mode leaves behind: `.` making a change made in it again
//! on as much text from the cursor, and `gv` selecting the last selection
//! again. Each case was checked against Neovim, save where clackatoo
//! differs on purpose (see [`super::super::visual`]).

use clackatoo_protocol::{BufferId, Size};

use super::{BufferCopy, Editor, Mode, Setup};
use crate::pane::Selection;

/// Six lines of eight characters, each different.
const GRID: &str = "abcdefgh\nijklmnop\nqrstuvwx\nyz012345\n6789ABCD\nEFGHIJKL\n";

/// The text after typing `keys` into [`GRID`].
fn grid_after(keys: &str) -> String {
    let mut setup = Setup::new(GRID);
    setup.keys(keys);
    assert_eq!(setup.editor.mode, Mode::Normal, "after {keys}");
    setup.text()
}

#[test]
fn dot_deletes_as_many_lines_again_and_as_many_characters_on_a_line() {
    let mut setup = Setup::new(GRID);
    setup.keys("Vjdj.");
    assert_eq!(setup.text(), "qrstuvwx\nEFGHIJKL\n");
    assert_eq!(setup.cursor(), (1, 0));
    let mut setup = Setup::new(GRID);
    setup.keys("lvlldj0.");
    assert_eq!(
        setup.text(),
        "aefgh\nlmnop\nqrstuvwx\nyz012345\n6789ABCD\nEFGHIJKL\n"
    );
    assert_eq!(setup.cursor(), (1, 0));
    // Again and again, as Neovim does, but for the line break, which a
    // selection never takes.
    assert_eq!(grid_after("lvlld.."), format!("a\n{}", &GRID[9..]));
}

#[test]
fn over_several_lines_dot_goes_as_many_lines_to_the_same_column() {
    // Whichever end the cursor was at.
    assert_eq!(
        grid_after("llvjldj0."),
        "abmnop\n2345\n6789ABCD\nEFGHIJKL\n"
    );
    assert_eq!(
        grid_after("jllllvkhdj0."),
        "abcnop\n345\n6789ABCD\nEFGHIJKL\n"
    );
    // From further along a line than the column, as far as it.
    assert_eq!(
        grid_after("llvjldj05l."),
        "abmnop\nqrstu2345\n6789ABCD\nEFGHIJKL\n"
    );
    // Taken to the end with `$`, it goes to the end of the last line.
    assert_eq!(grid_after("llvj$dj0l."), "ab\nq\n6789ABCD\nEFGHIJKL\n");
    assert_eq!(grid_after("llv$dj0l."), format!("ab\ni\n{}", &GRID[18..]));
}

#[test]
fn dot_counts_columns_so_a_tab_or_a_wide_character_takes_its_width() {
    // `a`, a tab to column 4 and `b`: five columns.
    let mut setup = Setup::new("a\tbcdefghijklmnop\nabcdefghijklmnopq\n");
    setup.keys("vlldj0.");
    assert_eq!(setup.text(), "cdefghijklmnop\nfghijklmnopq\n");
    // Three columns end in a wide character, which goes whole.
    let mut setup = Setup::new("abcdefgh\n日本語abcdef\n");
    setup.keys("vlldj0.");
    assert_eq!(setup.text(), "defgh\n語abcdef\n");
    // From a tab, they're counted from its last column, where the cursor
    // is drawn.
    let mut setup = Setup::new("abcdefgh\nx\tabcdefgh\n");
    setup.keys("vldj0l.");
    assert_eq!(setup.text(), "cdefgh\nxbcdefgh\n");
    // Ending on a tab, a selection over lines takes all of it.
    let mut setup = Setup::new("abcdefgh\nab\tcdefgh\nabcdefghijklmnop\nabcdefghijklmnop\n");
    setup.keys("lvjldj0.");
    assert_eq!(setup.text(), "acdefgh\nefghijklmnop\n");
}

#[test]
fn past_a_lines_end_dot_takes_no_line_break_and_past_the_last_line_no_more() {
    // Neovim takes the line break, joining the lines.
    let mut setup = Setup::new("abcdefgh\nab\nxyz\n");
    setup.keys("lvlllldj0.");
    assert_eq!(setup.text(), "agh\n\nxyz\n");
    let mut setup = Setup::new("a\nb\nc\nd\n");
    setup.keys("Vjjd.");
    assert_eq!(setup.text(), "");
}

#[test]
fn dot_changes_as_much_again_typing_what_was_typed_in_one_undo_step() {
    let mut setup = Setup::new(GRID);
    setup.keys("lvllcXY<Esc>j0.");
    assert_eq!(
        setup.text(),
        "aXYefgh\nXYlmnop\nqrstuvwx\nyz012345\n6789ABCD\nEFGHIJKL\n"
    );
    assert_eq!(setup.cursor(), (1, 1));
    assert_eq!(setup.editor.mode, Mode::Normal);
    setup.keys("u");
    assert_eq!(setup.text(), format!("aXYefgh\n{}", &GRID[9..]));
    let mut setup = Setup::new(GRID);
    setup.keys("llvjcZ<Esc>j0.");
    assert_eq!(setup.text(), "abZlmnop\nZ12345\n6789ABCD\nEFGHIJKL\n");
    // Whole lines keep the first's indent.
    let mut setup = Setup::new("  one\n  two\n  three\n  four\n");
    setup.keys("VjcX<Esc>j.");
    assert_eq!(setup.text(), "  X\n  X\n");
}

#[test]
fn a_copy_isnt_a_change_and_dot_makes_the_one_before() {
    assert_eq!(
        grid_after("lxvlly0j."),
        format!("acdefgh\njklmnop\n{}", &GRID[18..])
    );
}

#[test]
fn dot_indents_as_many_lines_by_as_many_steps_whatever_its_count() {
    let mut setup = Setup::new("a\nb\nc\nd\ne\n");
    setup.keys("Vj><Esc>j.");
    assert_eq!(setup.text(), "    a\n    b\n    c\n    d\ne\n");
    assert_eq!(setup.editor.mode, Mode::Normal, "the lines aren't kept");
    assert_eq!(setup.cursor(), (2, 4));
    // A count before `.` is ignored, as in Vim; the change keeps its own.
    let mut setup = Setup::new("a\nb\nc\nd\ne\n");
    setup.keys("Vj2><Esc>j3.");
    assert_eq!(
        setup.text(),
        "        a\n        b\n        c\n        d\ne\n"
    );
    setup.keys("Vj<lt><Esc>.");
    assert_eq!(setup.text(), "        a\n        b\n    c\nd\ne\n");
    // Where `>` keeps the lines selected, `.` indents them again.
    let mut setup = Setup::new("a\nb\nc\n");
    setup.keys("Vj>.");
    assert_eq!(setup.text(), "        a\n        b\nc\n");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Lines));
}

#[test]
fn dot_changes_the_case_of_as_much_again() {
    let upper = format!("aBCDefgh\nIJKlmnop\n{}", &GRID[18..]);
    assert_eq!(grid_after("lvll~j0."), upper);
    assert_eq!(grid_after("lvllUj0."), upper);
    assert_eq!(grid_after("lvllgUj0."), upper);
    let mut setup = Setup::new("ABCDEF\nGHIJKL\n");
    setup.keys("lvlluj0.");
    assert_eq!(setup.text(), "AbcdEF\nghiJKL\n");
    let mut setup = Setup::new("ABCDEF\nGHIJKL\n");
    setup.keys("lvllguj0.");
    assert_eq!(setup.text(), "AbcdEF\nghiJKL\n");
    assert_eq!(setup.cursor(), (1, 0));
    // Over lines, to the same column.
    assert_eq!(
        grid_after("6lvj~j0."),
        format!("abcdefGH\nijklmnoP\nQRSTUVWx\n{}", &GRID[27..])
    );
}

#[test]
fn visual_j_joins_the_lines_selected_and_dot_as_many() {
    let mut setup = Setup::new(GRID);
    setup.keys("VjjJ");
    assert_eq!(setup.editor.mode, Mode::Normal);
    assert_eq!(setup.cursor(), (0, 17));
    setup.keys("j.");
    assert_eq!(
        setup.text(),
        "abcdefgh ijklmnop qrstuvwx\nyz012345 6789ABCD EFGHIJKL\n"
    );
    assert_eq!(setup.cursor(), (1, 17));
    // One line joins with the next, and a count is ignored, as in Vim.
    assert_eq!(
        grid_after("VJj."),
        "abcdefgh ijklmnop\nqrstuvwx yz012345\n6789ABCD\nEFGHIJKL\n"
    );
    assert_eq!(
        grid_after("vj3J"),
        format!("abcdefgh ijklmnop\n{}", &GRID[18..])
    );
    // In one undo step.
    let mut setup = Setup::new(GRID);
    setup.keys("VjjJu");
    assert_eq!(setup.text(), GRID);
}

#[test]
fn visual_r_replaces_every_character_selected_and_dot_as_many() {
    let mut setup = Setup::new(GRID);
    setup.keys("lvllrx");
    assert_eq!(setup.editor.mode, Mode::Normal);
    assert_eq!(setup.cursor(), (0, 1));
    setup.keys("j0.");
    assert_eq!(setup.text(), format!("axxxefgh\nxxxlmnop\n{}", &GRID[18..]));
    assert_eq!(setup.cursor(), (1, 0));
    // But the line breaks; a tab is one character.
    assert_eq!(
        grid_after("llvjrxjj0."),
        "abxxxxxx\nxxxlmnop\nxxxxxxxx\nxxx12345\n6789ABCD\nEFGHIJKL\n"
    );
    let mut setup = Setup::new("a\tb\ncdefghijklm\n");
    setup.keys("vllrxj0.");
    assert_eq!(setup.text(), "xxx\nxxxxxhijklm\n");
    let mut setup = Setup::new("ab\ncd\nef\n");
    setup.keys("Vjr-");
    assert_eq!(setup.text(), "--\n--\nef\n");
}

#[test]
fn dot_puts_the_same_text_over_as_much_again() {
    // Neovim's only deletes it. After `p` or `P`, what was put, whatever
    // the register holds by then.
    let put = format!("abcdefgh\nabcdefghlmnop\nabcdefghtuvwx\n{}", &GRID[27..]);
    assert_eq!(grid_after("yiwj0vllPj0."), put);
    assert_eq!(grid_after("yiwj0vllpj0."), put);
    // Nothing to put is no change to make again.
    let mut setup = Setup::new(GRID);
    setup.keys("lvlldj0vl\"zp");
    assert_eq!(setup.editor.status.as_deref(), Some("register z is empty"));
    assert_eq!(setup.editor.mode, Mode::Normal);
    setup.keys("0.");
    assert_eq!(setup.text(), format!("aefgh\nlmnop\n{}", &GRID[18..]));
}
#[test]
fn delete_in_visual_mode_deletes_the_selection() {
    let mut setup = Setup::new("hello world\n");
    setup.keys("lvl<Del>");
    assert_eq!(setup.text(), "hlo world\n");
    assert_eq!(setup.editor.mode, Mode::Normal);
}

#[test]
fn in_visual_mode_dot_makes_only_a_change_made_in_visual_mode_again() {
    // On what's selected.
    let mut setup = Setup::new(GRID);
    setup.keys("lvlldjvl.");
    assert_eq!(setup.text(), format!("aefgh\nilmnop\n{}", &GRID[18..]));
    assert_eq!(setup.editor.mode, Mode::Normal);
    // A change made in normal mode isn't, as in Vim.
    let mut setup = Setup::new(GRID);
    setup.keys("xjvl.");
    assert_eq!(setup.text(), format!("bcdefgh\n{}", &GRID[9..]));
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Characters));
}

#[test]
fn a_repeated_visual_change_is_one_undo_step() {
    let mut setup = Setup::new(GRID);
    setup.keys("lvjlcQ<Esc>jj0.");
    let changed = setup.text();
    assert_eq!(changed, "aQlmnop\nqrstuvwx\nQ9ABCD\nEFGHIJKL\n");
    setup.keys("u");
    assert_eq!(
        setup.text(),
        "aQlmnop\nqrstuvwx\nyz012345\n6789ABCD\nEFGHIJKL\n"
    );
    setup.keys("<C-r>");
    assert_eq!(setup.text(), changed);
    setup.keys("uu");
    assert_eq!(setup.text(), GRID);
}

#[test]
fn gv_selects_the_last_selection_again() {
    let mut setup = Setup::new(GRID);
    setup.keys("gv");
    assert_eq!(setup.editor.mode, Mode::Normal, "nothing was selected");
    assert!(setup.editor.take_failed());
    setup.keys("lvjly0gv");
    assert_eq!(setup.editor.mode, Mode::Visual(Selection::Characters));
    setup.keys("d");
    assert_eq!(setup.text(), format!("almnop\n{}", &GRID[18..]));
    // As it was selected, whole lines, ended by Esc.
    assert_eq!(grid_after("Vj<Esc>jjjgvd"), &GRID[18..]);
    assert_eq!(
        grid_after("lvl<Esc>jjgvd"),
        format!("adefgh\n{}", &GRID[9..])
    );
}

#[test]
fn gv_puts_the_cursor_at_the_end_it_was_at() {
    let mut setup = Setup::new(GRID);
    setup.keys("lvjly0gv");
    assert_eq!(setup.cursor(), (1, 2));
    let mut setup = Setup::new(GRID);
    setup.keys("jlllvkhy0gv");
    assert_eq!(setup.cursor(), (0, 2));
    assert_eq!(
        setup.editor.anchor.map(|at| (at.line, at.col)),
        Some((1, 3))
    );
    // Kept to the line's end, after `$`.
    let mut setup = Setup::new("short\nmuch longer\n");
    setup.keys("v$y0gvj");
    assert_eq!(setup.cursor(), (1, 10));
}

#[test]
fn gv_keeps_to_the_characters_selected_through_edits() {
    // Lines put in above take it down with them.
    assert_eq!(
        grid_after("jlvjlyggOnew<Esc>gvd"),
        format!("new\nabcdefgh\nituvwx\n{}", &GRID[27..])
    );
    // Text typed before it in its line takes it along, where Neovim's
    // stays at its column.
    assert_eq!(
        grid_after("lvjly0iZZ<Esc>gvd"),
        format!("ZZalmnop\n{}", &GRID[18..])
    );
    // Text typed in it is in it.
    assert_eq!(
        grid_after("lvjly0jiZZ<Esc>gvd"),
        format!("almnop\n{}", &GRID[18..])
    );
    // Deleted, it's where it was.
    let mut setup = Setup::new(GRID);
    setup.keys("lvjldgv");
    assert_eq!(
        setup.editor.anchor.map(|at| (at.line, at.col)),
        Some((0, 1))
    );
    assert_eq!(setup.cursor(), (0, 1));
}

#[test]
fn gv_follows_edits_from_elsewhere_and_undo_and_the_text_made_anew() {
    let mut setup = Setup::new(GRID);
    setup.keys("jlvly");
    let update = setup.daemon.agent_replace(0, "", "top\n");
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(update);
    setup.heard_marks(&heard);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    setup.keys("gvd");
    assert_eq!(
        setup.text(),
        format!("top\nabcdefgh\nilmnop\n{}", &GRID[18..])
    );
    setup.keys("gg0jlvly");
    // Read again, it's where it was, by line and column, as marks are.
    let replaced = super::super::Daemon::new("other\nlines here\n").snapshot();
    setup.editor.hold(&setup.copy);
    let mut heard = setup.copy.update(replaced);
    setup.heard_marks(&heard);
    setup.editor.heard(&mut heard, false, &mut setup.copy);
    setup.keys("gv");
    assert_eq!(
        setup.editor.anchor.map(|at| (at.line, at.col)),
        Some((1, 1))
    );
    assert_eq!(setup.cursor(), (1, 2));
}

#[test]
fn gv_after_an_undo_selects_all_that_was_put_back() {
    assert_eq!(grid_after("lvlldugvd"), format!("aefgh\n{}", &GRID[9..]));
    assert_eq!(grid_after("jVjdugvd"), format!("abcdefgh\n{}", &GRID[27..]));
}
#[test]
fn in_visual_mode_gv_swaps_with_the_last_selection() {
    assert_eq!(grid_after("lvyjjvlgvd"), format!("acdefgh\n{}", &GRID[9..]));
    let swapped = format!("abcdefgh\nijklmnop\nqtuvwx\n{}", &GRID[27..]);
    assert_eq!(grid_after("lvlyjjvlgvgvd"), swapped);
    // Selected by `gv`, the last is the same: `gv` again stays.
    assert_eq!(grid_after("lvlyjjgvgvd"), format!("adefgh\n{}", &GRID[9..]));
    // In its own mode.
    assert_eq!(grid_after("Vyjjvlgvd"), &GRID[9..]);
}

#[test]
fn the_selections_marks_stay_on_characters_whose_case_changes() {
    let mut setup = Setup::new(GRID);
    setup.keys("lvl~0");
    let marked = |setup: &Setup, mark| {
        let place = setup.shared.marks.get(mark, setup.copy.path()).unwrap();
        (place.line, place.col)
    };
    assert_eq!((marked(&setup, '<'), marked(&setup, '>')), ((0, 1), (0, 2)));
    setup.keys("g~~");
    assert_eq!((marked(&setup, '<'), marked(&setup, '>')), ((0, 1), (0, 2)));
    setup.keys("gvd");
    assert_eq!(setup.text(), format!("ADEFGH\n{}", &GRID[9..]));
}

#[test]
fn dot_neither_selects_nor_marks() {
    let mut setup = Setup::new("a\nb\nc\nd\ne\n");
    setup.keys("Vj><Esc>j.");
    assert_eq!(setup.text(), "    a\n    b\n    c\n    d\ne\n");
    let line = |setup: &Setup, mark| {
        let place = setup.shared.marks.get(mark, setup.copy.path()).unwrap();
        place.line
    };
    assert_eq!((line(&setup, '<'), line(&setup, '>')), (0, 1));
    setup.keys("gvd");
    assert_eq!(setup.text(), "    c\n    d\ne\n");
    // Nor does a case changed move the selection off its characters.
    assert_eq!(
        grid_after("lvl~jj0.gvd"),
        format!("adefgh\nijklmnop\nQRstuvwx\n{}", &GRID[27..])
    );
}

#[test]
fn a_files_editors_share_its_last_selection_and_each_file_keeps_its_own() {
    let mut setup = Setup::new(GRID);
    let mut other = Editor::new(BufferId(1), &setup.options);
    other.resize(Size::new(40, 10).unwrap());
    setup.keys("lvly");
    // Another file's selection is its own.
    let mut elsewhere = BufferCopy::scratch(BufferId(2), "/elsewhere.txt", "xyz\n");
    let mut third = Editor::new(BufferId(2), &setup.options);
    third.resize(Size::new(40, 10).unwrap());
    for stroke in super::Stroke::read("vly<Esc>") {
        third.typed(stroke, &mut elsewhere, &mut setup.shared, false);
    }
    // As the desk has the other editor follow the edits one makes.
    for stroke in super::Stroke::read("jjgvd") {
        other.typed(stroke, &mut setup.copy, &mut setup.shared, false);
    }
    for edit in &setup.copy.take_made() {
        setup.editor.follow_edit(edit, &setup.copy);
    }
    setup.serve();
    assert_eq!(setup.text(), format!("adefgh\n{}", &GRID[9..]));
    setup.keys("jj0vllygg");
    for stroke in super::Stroke::read("gvd") {
        other.typed(stroke, &mut setup.copy, &mut setup.shared, false);
    }
    setup.serve();
    assert_eq!(
        setup.text(),
        format!("adefgh\nijklmnop\ntuvwx\n{}", &GRID[27..])
    );
}

/// A folder's listing, as the explorer makes one to edit: a line without
/// a mark, as `..` is, then three entries, each line's mark hidden.
pub(super) fn listing() -> (Setup, [String; 3]) {
    use crate::explorer::listing::{mark, mark_len};
    let marks = [mark(1, 0), mark(1, 1), mark(1, 2)];
    let [one, two, three] = &marks;
    let mut setup = Setup::new(&format!("..\n{one}alpha\n{two}beta/\n{three}gamma\n"));
    setup.editor.hide_starts(mark_len);
    setup.editor.keep_lines(crate::explorer::listing::is_up);
    (setup, marks)
}

#[test]
fn visual_r_and_case_changes_leave_a_listings_hidden_marks_as_they_are() {
    let (mut setup, [one, two, three]) = listing();
    setup.keys("jVjr-");
    assert_eq!(
        setup.text(),
        format!("..\n{one}-----\n{two}-----\n{three}gamma\n")
    );
    let (mut setup, [one, two, three]) = listing();
    setup.keys("jlvjrx");
    assert_eq!(
        setup.text(),
        format!("..\n{one}axxxx\n{two}xxta/\n{three}gamma\n")
    );
    // And `.`, on the next lines.
    setup.keys("j0vlr-j0.");
    assert_eq!(
        setup.text(),
        format!("..\n{one}axxxx\n{two}--ta/\n{three}--mma\n")
    );
    // `..` stays `..`, which names nothing, however `.` comes to it.
    let (mut setup, [one, two, three]) = listing();
    setup.keys("ggVjr-");
    assert_eq!(
        setup.text(),
        format!("..\n{one}-----\n{two}beta/\n{three}gamma\n")
    );
    setup.keys("jj0vlr*gg0.");
    assert_eq!(
        setup.text(),
        format!("..\n{one}-----\n{two}**ta/\n{three}gamma\n")
    );
    for (keys, case) in [("U", "ALPHA\n{}Beta/"), ("g?", "nycun\n{}oeta/")] {
        let (mut setup, [one, two, three]) = listing();
        setup.keys(&format!("jvj{keys}"));
        let case = case.replace("{}", two.as_str());
        assert_eq!(setup.text(), format!("..\n{one}{case}\n{three}gamma\n"));
    }
}

#[test]
fn joining_a_listings_lines_is_refused_and_whole_lines_go() {
    let refused = |keys: &str| {
        let (mut setup, [one, two, three]) = listing();
        setup.keys(keys);
        let text = format!("..\n{one}alpha\n{two}beta/\n{three}gamma\n");
        assert_eq!(setup.text(), text, "{keys}");
        assert_eq!(
            setup.editor.status.as_deref(),
            Some(super::super::visual::JOINS_ENTRIES)
        );
        assert_eq!(setup.editor.mode, Mode::Normal, "{keys}");
    };
    // `J`, in either mode, even onto `..`, nor `..` onto a line with no
    // mark.
    refused("jVjJ");
    refused("jJ");
    refused("J");
    let (mut setup, [one, _, _]) = listing();
    setup.keys("onew<Esc>VkJ");
    assert_eq!(
        setup.editor.status.as_deref(),
        Some(super::super::visual::JOINS_ENTRIES)
    );
    assert!(setup.text().starts_with(&format!("..\nnew\n{one}")));
    // Characters over lines, deleted, changed or put over.
    refused("jlvjd");
    refused("jlvj<Del>");
    refused("jlvjc");
    refused("yiwjlvjp");
    // Within a line, and whole lines, they're done.
    let (mut setup, [one, two, three]) = listing();
    setup.keys("jlvl<Del>");
    assert_eq!(
        setup.text(),
        format!("..\n{one}aha\n{two}beta/\n{three}gamma\n")
    );
    setup.keys("jVjd");
    assert_eq!(setup.text(), format!("..\n{one}aha\n"));
    let (mut setup, [one, two, three]) = listing();
    setup.keys("jyyjVp");
    assert_eq!(
        setup.text(),
        format!("..\n{one}alpha\n{one}alpha\n{three}gamma\n")
    );
    let _ = two;
    // A change refused isn't one `.` makes again.
    let (mut setup, [one, two, three]) = listing();
    setup.keys("jvlr-jlvjd0.");
    assert_eq!(
        setup.text(),
        format!("..\n{one}--pha\n{two}beta/\n{three}--mma\n")
    );
}

#[test]
fn indenting_a_listings_lines_is_refused() {
    let refused = |keys: &str| {
        let (mut setup, [one, two, three]) = listing();
        setup.keys(keys);
        let text = format!("..\n{one}alpha\n{two}beta/\n{three}gamma\n");
        assert_eq!(setup.text(), text, "{keys}");
        assert_eq!(
            setup.editor.status.as_deref(),
            Some(super::super::visual::INDENTS_ENTRIES),
            "{keys}"
        );
        assert_eq!(setup.editor.mode, Mode::Normal, "{keys}");
    };
    // `..` too, which has no mark, alone or with entries.
    for keys in [
        "j>>", "j<<", "j2>>", "j>j", ">>", "<<", "Vj>", "jVj>", "jVj<", "jVj3>", "jvl>",
    ] {
        refused(keys);
    }
    // A refused one isn't a change for `.`: it makes the one before again.
    let (mut setup, [one, two, three]) = listing();
    setup.keys("jx>>j.");
    assert_eq!(
        setup.text(),
        format!("..\n{one}lpha\n{two}eta/\n{three}gamma\n")
    );
}
