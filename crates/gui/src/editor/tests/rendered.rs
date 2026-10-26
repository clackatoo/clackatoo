//! A Markdown file rendered: turned on and off keeping the place, read
//! with the view's keys, turned back into its text by any other, and kept
//! up with edits made elsewhere, laying out again only what they change.

use clackatoo_protocol::{BufferId, BufferUpdate};

use super::Setup;
use crate::editor::Editor;
use crate::fonts::Fonts;
use crate::options::{MarkdownView, Options};
use crate::vim::Mode;

/// A pane 800 by 480 pixels, as the tests draw it in.
const SIZE: [f32; 2] = [800.0, 480.0];

/// An editor of `text`, a Markdown file's, which keeps no lines in view
/// around the cursor, so `zt` puts its line at the very top.
fn markdown(text: &str) -> Setup {
    let mut setup = Setup::new(text);
    setup.copy.update(BufferUpdate::Moved("/p/notes.md".into()));
    setup.set(|options| options.scrolloff = 0);
    setup
}

/// A document of `parts` parts, each a heading and a paragraph: four lines
/// a part, the heading of part `n` on line `4n`.
fn document(parts: usize) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    for part in 0..parts {
        let _ = write!(
            text,
            "## Part {part}\n\nThe words of part {part}, which say little.\n\n"
        );
    }
    text
}

/// Turns the rendered view on, or off.
fn toggle(setup: &mut Setup) {
    setup
        .editor
        .toggle_rendered(&mut setup.copy)
        .expect("it's Markdown");
}

/// Draws the rendered view, as the window does each frame.
fn paint(setup: &mut Setup, fonts: &mut Fonts) {
    let painted = (setup.editor).paint_rendered(fonts, [0.0; 2], SIZE, &setup.copy);
    assert!(painted.is_some(), "it's rendered");
}

/// The lines the blocks in view start on, as drawn last: the first and
/// the last.
fn shown_lines(setup: &Setup) -> (usize, usize) {
    let rendered = setup.editor.rendered.as_ref().expect("it's rendered");
    let (first, last) = rendered.shown_bytes().expect("it's been drawn");
    let rope = setup.copy.text();
    (rope.byte_to_line(first), rope.byte_to_line(last))
}

#[test]
fn only_markdown_renders_and_the_bar_says_why_not() {
    let mut setup = Setup::new("fn main() {}\n");
    let refused = setup.editor.toggle_rendered(&mut setup.copy);
    assert_eq!(refused.unwrap_err(), "‘a’ isn't Markdown");
    assert!(!setup.editor.rendered());
    let mut setup = markdown("# Notes\n");
    toggle(&mut setup);
    assert!(setup.editor.rendered());
    assert_eq!(setup.editor.label(&setup.copy), "rendered Markdown");
    toggle(&mut setup);
    assert!(!setup.editor.rendered());
    assert_eq!(setup.editor.label(&setup.copy), "raw Markdown");
}

#[test]
fn it_opens_at_the_block_at_the_top_and_the_text_comes_back_at_the_block_then_at_the_top() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    // Part 10's paragraph at the top of the view.
    setup.keys("43Gzt");
    assert_eq!(setup.view().0, 42);
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    assert_eq!(shown_lines(&setup).0, 42, "the block holding the top line");
    // The text again, with no scrolling: as it was.
    toggle(&mut setup);
    assert_eq!(setup.view(), (42, 42, 0));
    // Scrolled on, the text comes back at the block that was at the top,
    // and the cursor, out of view, goes to it.
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("<C-d><C-d>");
    paint(&mut setup, &mut fonts);
    let (top, _) = shown_lines(&setup);
    assert!(top > 42, "scrolled from line 42 to {top}");
    toggle(&mut setup);
    assert_eq!(setup.view(), (top as u64, top as u64, 0));
}

#[test]
fn the_views_keys_scroll_it_and_the_bar_says_how_far() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    let label = |setup: &Setup| setup.editor.label(&setup.copy);
    // Esc says nothing, as what the bar said goes.
    setup.keys("<Esc>");
    assert_eq!(label(&setup), "notes.md  rendered  Top");
    setup.keys("3j");
    paint(&mut setup, &mut fonts);
    assert!(label(&setup).ends_with('%'), "{}", label(&setup));
    setup.keys("G");
    paint(&mut setup, &mut fonts);
    assert_eq!(label(&setup), "notes.md  rendered  Bot");
    assert_eq!(shown_lines(&setup).1, 4 * 59 + 2, "the last paragraph");
    setup.keys("gg");
    paint(&mut setup, &mut fonts);
    assert_eq!(label(&setup), "notes.md  rendered  Top");
    // A count goes that many lines; with `G`, to the block of that line.
    setup.keys("81G");
    paint(&mut setup, &mut fonts);
    assert_eq!(shown_lines(&setup).0, 80);
    setup.keys("gg41gg");
    paint(&mut setup, &mut fonts);
    assert_eq!(shown_lines(&setup).0, 40, "as with G");
    setup.keys("81G");
    paint(&mut setup, &mut fonts);
    setup.keys("<C-u>");
    paint(&mut setup, &mut fonts);
    assert!(shown_lines(&setup).0 < 80);
    assert!(label(&setup).ends_with('%'), "{}", label(&setup));
    // The wheel scrolls it too.
    let before = shown_lines(&setup).0;
    assert!(setup.editor.wheel_rendered(-10));
    paint(&mut setup, &mut fonts);
    assert!(shown_lines(&setup).0 > before);
    assert!(setup.editor.rendered(), "none of these show the text");
}

#[test]
fn any_other_key_shows_the_text_and_does_there_what_it_does() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("81G");
    paint(&mut setup, &mut fonts);
    // `i` types where the view was: at the start of its top block.
    setup.keys("i");
    assert!(!setup.editor.rendered());
    assert_eq!(setup.editor.mode, Mode::Insert);
    setup.keys("Hello <Esc>");
    assert!(setup.text().contains("\nHello ## Part 20\n"));
    // A count before the key goes with it: `2dd` deletes two lines.
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("2dd");
    assert!(!setup.editor.rendered());
    assert!(!setup.text().contains("Hello"));
    assert!(setup.text().contains("\n\nThe words of part 20,"));
}

#[test]
fn edits_made_elsewhere_lay_out_again_only_what_they_change_and_the_view_stays_put() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    setup.keys("41Gzt");
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    assert_eq!(shown_lines(&setup).0, 40);
    let fresh = |setup: &Setup| setup.editor.rendered.as_ref().unwrap().fresh;
    assert_eq!(fresh(&setup), 120, "every block, the first time");
    // An agent adds a word to part 30's paragraph, and a paragraph at the
    // top.
    let edit = |setup: &mut Setup, at: usize, text: &str| {
        let update = setup.daemon.agent(at, text);
        setup.editor.hold(&setup.copy);
        let mut heard = setup.copy.update(update);
        setup.editor.heard(&mut heard, false, &mut setup.copy);
    };
    let thirty = setup.text().find("part 30,").unwrap();
    edit(&mut setup, thirty, "all of ");
    paint(&mut setup, &mut fonts);
    assert_eq!(fresh(&setup), 1, "only the paragraph that changed");
    edit(&mut setup, 0, "An introduction.\n\n");
    paint(&mut setup, &mut fonts);
    assert_eq!(fresh(&setup), 1, "only the paragraph that's new");
    assert_eq!(shown_lines(&setup).0, 42, "the same block, two lines down");
    // Drawn again with nothing changed, nothing is laid out, or read.
    paint(&mut setup, &mut fonts);
    assert_eq!(fresh(&setup), 1);
}

#[test]
fn a_link_to_a_heading_scrolls_to_it_and_links_are_found_where_drawn() {
    let mut fonts = crate::chat::fixtures::fonts();
    let text = format!(
        "See [the end](#part-50) or [docs](https://x.org).\n\n{}",
        document(60)
    );
    let mut setup = markdown(&text);
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    // Across the first line of text, the links' text is where they are.
    let rendered = setup.editor.rendered.as_ref().unwrap();
    let mut found: Vec<&str> = (0..800_u16)
        .step_by(2)
        .filter_map(|x| rendered.link_at([f32::from(x), 40.0]))
        .collect();
    found.dedup();
    assert_eq!(found, ["#part-50", "https://x.org"]);
    assert!(setup.editor.rendered_heading("part-50"));
    paint(&mut setup, &mut fonts);
    assert_eq!(shown_lines(&setup).0, 2 + 4 * 50);
    assert!(!setup.editor.rendered_heading("part-99"));
}

#[test]
fn headings_are_named_as_github_names_them() {
    assert_eq!(
        super::super::rendered::slug("Getting started"),
        "getting-started"
    );
    assert_eq!(
        super::super::rendered::slug("What's new? (v2)"),
        "whats-new-v2"
    );
    assert_eq!(
        super::super::rendered::slug("snake_case and-dash"),
        "snake_case-and-dash"
    );
}

/// What it costs to draw a long README, a few thousand lines of headings,
/// prose, lists, code and tables: first, with nothing changed, as it
/// scrolls, and as an agent types into it elsewhere, a key a frame.
/// Prints the times; it asserts nothing.
#[test]
#[ignore = "a measurement: run with --release --ignored --nocapture"]
fn frame_time_on_a_long_markdown_file() {
    use std::time::Instant;

    let part = |n: usize| {
        format!(
            "## Section {n}\n\nSome **prose** for section {n}, with `code`, a [link](https://x.org/{n}) \
             and enough words to wrap onto a second line in a pane of the usual width.\n\n\
             - one thing\n- another, with *emphasis*\n  - and a nested one\n\n\
             ```json\n{{ \"section\": {n}, \"ok\": true }}\n```\n\n\
             | Name | Value |\n|---|---|\n| a | {n} |\n| b | {n} |\n\n> A quote, {n}.\n\n"
        )
    };
    let text: String = (0..160).map(part).collect();
    let mut setup = markdown(&text);
    println!(
        "{} lines, {} KiB",
        setup.copy.text().len_lines(),
        text.len() / 1024
    );
    let mut fonts = crate::chat::fixtures::fonts();
    let frames = 100;
    toggle(&mut setup);
    let start = Instant::now();
    paint(&mut setup, &mut fonts);
    println!("first frame: {:?}", start.elapsed());
    let start = Instant::now();
    for _ in 0..frames {
        paint(&mut setup, &mut fonts);
    }
    println!("nothing changed: {:?} a frame", start.elapsed() / frames);
    let start = Instant::now();
    for _ in 0..frames {
        setup.keys("<C-d>");
        paint(&mut setup, &mut fonts);
    }
    println!("scrolling: {:?} a frame", start.elapsed() / frames);
    let middle = text.len() / 2 + text[text.len() / 2..].find("enough").unwrap();
    let start = Instant::now();
    for typed in 0..frames {
        let update = setup.daemon.agent(middle + typed as usize, "x");
        let mut heard = setup.copy.update(update);
        setup.editor.heard(&mut heard, false, &mut setup.copy);
        paint(&mut setup, &mut fonts);
    }
    println!(
        "typed elsewhere: {:?} a frame, {} block laid out afresh",
        start.elapsed() / frames,
        setup.editor.rendered.as_ref().unwrap().fresh
    );
    // Its parts: reading the text, and laying out what's kept.
    let whole = setup.copy.text().to_string();
    let start = Instant::now();
    for _ in 0..frames {
        std::hint::black_box(crate::markdown::blocks(&whole));
    }
    println!("of which reading it: {:?}", start.elapsed() / frames);
}

#[test]
fn markdownview_opens_markdown_files_rendered_once_theyve_come() {
    let options = Options {
        markdownview: MarkdownView::Rendered,
        ..Options::default()
    };
    let mut setup = markdown("# Notes\n");
    setup.editor = Editor::new(BufferId(1), &options);
    setup.editor.settle(&setup.copy);
    assert!(setup.editor.rendered());
    // Only once: turned off, it stays off.
    toggle(&mut setup);
    setup.editor.settle(&setup.copy);
    assert!(!setup.editor.rendered());
    // And never for a file that isn't Markdown.
    let mut setup = Setup::new("fn main() {}\n");
    setup.editor = Editor::new(BufferId(1), &options);
    setup.editor.settle(&setup.copy);
    assert!(!setup.editor.rendered());
}

#[test]
fn markdownview_leaves_a_file_as_its_text_while_a_command_is_half_typed() {
    let options = Options {
        markdownview: MarkdownView::Rendered,
        ..Options::default()
    };
    // A search being typed as the text comes, a change waiting for its
    // object, and a count: each goes on in the text, as typed.
    for (before, after, text) in [
        ("/Ins", "tall<CR>x", "# Notes\n\nTo nstall it.\n"),
        ("ci", "w<Esc>", "# \n\nTo Install it.\n"),
        ("3", "x", "# es\n\nTo Install it.\n"),
    ] {
        let mut setup = markdown("# Notes\n\nTo Install it.\n");
        setup.editor = Editor::new(BufferId(1), &options);
        setup.keys(&format!("w{before}"));
        setup.editor.settle(&setup.copy);
        assert!(!setup.editor.rendered(), "{before}");
        setup.keys(after);
        assert_eq!(setup.text(), text, "{before}{after}");
        // And for good: it stays its text.
        setup.editor.settle(&setup.copy);
        assert!(!setup.editor.rendered());
    }
}

#[test]
fn a_count_or_g_typed_in_the_view_is_the_editors_and_goes_as_any_does() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("3");
    assert_eq!(setup.editor.counting(), Some(3));
    assert!(
        setup
            .editor
            .label(&setup.copy)
            .starts_with("3  notes.md  rendered")
    );
    // Leaving the pane forgets it, as it does a count typed in the text.
    setup.editor.leave();
    assert!(!setup.editor.pending());
    setup.keys("x");
    assert!(
        setup.text().starts_with("# Part 0\n"),
        "one character, not three"
    );
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("g");
    assert!(setup.editor.pending());
    setup.editor.forget_typed();
    setup.keys("i");
    assert_eq!(setup.editor.mode, Mode::Insert);
}

#[test]
fn a_new_size_forgets_how_far_a_count_set_ctrl_d_to_go() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    let scrolled = |setup: &Setup| setup.editor.rendered.as_ref().unwrap().scrolled();
    setup.keys("1<C-d>");
    let line = scrolled(&setup);
    assert!(line > 0.0);
    setup.keys("<C-d>");
    assert!(
        (scrolled(&setup) - 2.0 * line).abs() < 0.5,
        "the count kept"
    );
    // Taller, half the view again.
    let taller = [SIZE[0], SIZE[1] + 10.0 * line];
    (setup.editor).paint_rendered(&mut fonts, [0.0; 2], taller, &setup.copy);
    setup.keys("<C-d>");
    assert!(scrolled(&setup) > 4.0 * line, "{}", scrolled(&setup));
}

#[test]
fn gj_and_gk_scroll_as_j_and_k_do() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("<Esc>20gj");
    paint(&mut setup, &mut fonts);
    assert!(setup.editor.rendered());
    let down = shown_lines(&setup).0;
    assert!(down > 0, "scrolled down to line {down}");
    setup.keys("20gk");
    paint(&mut setup, &mut fonts);
    assert!(setup.editor.rendered());
    assert_eq!(shown_lines(&setup).0, 0);
}

#[test]
fn a_key_that_edits_does_so_at_the_top_block_not_at_a_cursor_never_drawn() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    // The cursor on line 2, in view; the view not scrolled.
    setup.keys("2j");
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("x");
    assert!(!setup.editor.rendered());
    assert!(
        setup.text().starts_with("# Part 0\n\nThe words"),
        "{}",
        setup.text()
    );
    // And a paste goes there too.
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    setup.keys("41G");
    paint(&mut setup, &mut fonts);
    setup.editor.paste("pasted ", &mut setup.copy);
    setup.serve();
    assert!(setup.text().contains("\npasted ## Part 10\n"));
}

#[test]
fn a_link_in_a_tables_cell_cut_off_at_the_columns_edge_isnt_clicked() {
    let mut fonts = crate::chat::fixtures::fonts();
    let wide = "a long cell that is wider than the narrow column it's drawn in";
    let text = format!("| One | Two |\n|---|---|\n| {wide} | [hidden](https://x.org) |\n");
    let mut setup = markdown(&text);
    toggle(&mut setup);
    let painted = (setup.editor).paint_rendered(&mut fonts, [0.0; 2], [300.0, 480.0], &setup.copy);
    assert!(painted.is_some());
    let rendered = setup.editor.rendered.as_ref().unwrap();
    let found = (0..200_u16)
        .flat_map(|x| (0..120_u16).map(move |y| [f32::from(x) * 4.0, f32::from(y) * 4.0]))
        .find_map(|at| rendered.link_at(at));
    assert_eq!(found, None);
}

#[test]
fn renamed_to_something_that_isnt_markdown_it_shows_its_text() {
    let mut setup = markdown("# Notes\n");
    toggle(&mut setup);
    setup
        .copy
        .update(BufferUpdate::Moved("/p/notes.txt".into()));
    setup.editor.settle(&setup.copy);
    assert!(!setup.editor.rendered());
}

#[test]
fn the_views_keys_are_read_by_the_grammar_and_its_help_comes_from_it() {
    let mut fonts = crate::chat::fixtures::fonts();
    let mut setup = markdown(&document(60));
    toggle(&mut setup);
    paint(&mut setup, &mut fonts);
    // Ctrl+E with a count, as in any pane.
    setup.keys("5<C-e>");
    paint(&mut setup, &mut fonts);
    assert!(shown_lines(&setup).0 > 0, "scrolled");
    assert!(setup.editor.rendered());
    // `g` and Esc forgets it, and stays.
    setup.keys("g<Esc>");
    assert!(setup.editor.rendered() && !setup.editor.pending());
    let rows = super::super::rendered::shortcuts(&setup.editor.keys).rows;
    let has = |key: &str| rows.iter().any(|(k, _)| k == key);
    assert!(
        has("j") && has("gg") && has("Ctrl+D") && has("G"),
        "{rows:?}"
    );
    assert!(!has("w") && !has("dd"), "the text's keys aren't the view's");
    assert!(has("Other editor keys"));
    // A register is the text's: it comes back, to have it.
    setup.keys("\"a");
    assert!(setup.editor.rendered(), "waiting for the rest");
    setup.keys("yy");
    assert!(!setup.editor.rendered());
    // At the line the top block starts on.
    assert_eq!(setup.held('a').as_deref(), Some("## Part 1\n"));
}
