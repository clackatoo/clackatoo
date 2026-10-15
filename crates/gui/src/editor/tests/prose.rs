//! Lines drawn in styles of the editor's own: a lesson's prose, and a
//! folder's listing.

use clackatoo_protocol::{Attributes, Color, Size, Style};

use super::super::{Lit, Prose, Stretch};
use super::Setup;
use crate::render::HighlightKind;

/// The rows drawn, as text, without the `~` ones after the text.
fn rows(setup: &mut Setup) -> Vec<String> {
    setup.editor.settle(&setup.copy);
    (setup.editor.scene().0.iter())
        .map(|row| row.text())
        .take_while(|row| !row.starts_with('~'))
        .collect()
}

/// The style of the cell at `col` of row `row`, as last drawn.
fn style_at(setup: &Setup, row: usize, col: usize) -> Style {
    let rows = setup.editor.scene().0;
    *rows[row].style(rows[row].cells()[col])
}

/// Types `keys` straight into a listing's copy, which, as the window's own,
/// needs no daemon, and hands it what it answers itself, as the desk does.
fn typed(setup: &mut Setup, keys: &str) {
    for stroke in super::super::Stroke::read(keys) {
        (setup.editor).typed(stroke, &mut setup.copy, &mut setup.shared, false);
        loop {
            let answers = setup.copy.take_answers();
            if answers.is_empty() {
                break;
            }
            for answer in answers {
                setup.copy.update(answer);
            }
        }
    }
}

const BOLD: Style = Style {
    attributes: Attributes::BOLD,
    ..Style::DEFAULT
};

#[test]
fn a_lesson_is_prose_with_bold_titles_code_kept_whole_and_ticks_by_steps_done() {
    let title = "1. Find it";
    // In 28 cells, `:Explore DIR` would break after `:Explore`.
    let body = "   See it all, then :Explore DIR here.";
    let mut setup = Setup::new(&format!("{title}\n{body}\n2. Next\n"));
    setup.editor.resize(Size::new(30, 10).unwrap());
    let start = title.len() + 1;
    let code = start + body.find(":Explore").unwrap();
    let prose = Prose {
        styled: vec![Stretch {
            start: 0,
            end: title.len(),
            style: BOLD,
        }],
        code: vec![(code, code + ":Explore DIR".len())],
    };
    let options = setup.options.clone();
    setup.editor.show_lesson(&options, &setup.copy, prose);
    setup.editor.light(vec![Lit {
        lines: 0..1,
        kind: HighlightKind::LessonDone,
        until: None,
    }]);
    // A column for ticks, then the text, wrapped between words, kept
    // whole where it's code.
    assert_eq!(
        rows(&mut setup),
        [
            "✓ 1. Find it",
            "     See it all, then",
            "     :Explore DIR here.",
            "  2. Next",
        ]
    );
    assert_eq!(
        style_at(&setup, 0, 0).fg,
        Color::Indexed(10),
        "a green tick"
    );
    assert_eq!(style_at(&setup, 0, 5), BOLD, "the title");
    assert_eq!(style_at(&setup, 1, 5), Style::DEFAULT, "the body");
    // The code is lit as code is, where it's drawn.
    let lit = setup.highlighted(HighlightKind::Code);
    assert_eq!(lit, [(2, 5..17)]);
    // A step not done has no tick.
    setup.editor.light(Vec::new());
    assert!(rows(&mut setup)[0].starts_with("  1."));
}

#[test]
fn the_cursor_on_a_blank_hanging_past_a_lessons_row_is_drawn_at_its_edge() {
    // 28 cells of text fill a row of a pane 30 wide, less the ticks'
    // column; the blank after them hangs past its edge.
    let body = format!("   {} bbb", "a".repeat(25));
    let mut setup = Setup::new(&format!("{body}\n"));
    setup.editor.resize(Size::new(30, 10).unwrap());
    let options = setup.options.clone();
    setup
        .editor
        .show_lesson(&options, &setup.copy, Prose::default());
    setup.keys("28l");
    assert_eq!(setup.cursor(), (0, 28), "on the blank");
    assert_eq!(
        rows(&mut setup)[1],
        "     bbb",
        "the next row starts with the word"
    );
    let cursor = setup.editor.scene().1.expect("a cursor drawn");
    assert_eq!((cursor.line, cursor.col), (0, 29), "at the row's last cell");
}

#[test]
fn a_listing_colours_its_folders_as_the_explorer_does_and_says_when_its_empty() {
    use crate::explorer::listing;
    let listing = |text: &str| {
        let mut setup = Setup::new(text);
        // Modified, as a listing's copy is, once it says something else
        // than it did: not by its `..` alone.
        setup.copy = super::super::BufferCopy::scratch_judged(
            clackatoo_protocol::BufferId(1),
            "/p",
            text,
            listing::same_rope,
        );
        setup.set(|options| {
            options.number = false;
            options.relativenumber = false;
        });
        setup.editor.keep_lines(listing::is_up);
        setup.editor.style_lines(listing::style);
        setup.editor.note_when_empty(listing::EMPTY);
        setup
    };
    let mut setup = listing("..\nsub/\nfile.rs\n");
    assert_eq!(rows(&mut setup), ["..", "sub/", "file.rs"]);
    // Emptied by hand, it isn't empty yet: `:w` would delete all it had.
    let mut emptied = listing("..\nsub/\nfile.rs\n");
    typed(&mut emptied, "ggjdG");
    emptied.editor.settle(&emptied.copy);
    let shown: Vec<String> = (emptied.editor.scene().0.iter())
        .take(2)
        .map(|row| row.text())
        .collect();
    assert_eq!(shown, ["..", "~"], "{}", emptied.copy.text());
    let folder = Style {
        fg: Color::Indexed(4),
        attributes: Attributes::BOLD,
        ..Style::DEFAULT
    };
    assert_eq!(style_at(&setup, 0, 0), folder, "the way up");
    assert_eq!(style_at(&setup, 1, 3), folder);
    assert_eq!(style_at(&setup, 2, 0), Style::DEFAULT, "a file");
    // With nothing but `..`, it says how to make a file, under it.
    let mut setup = listing("..\n");
    setup.editor.settle(&setup.copy);
    let shown: Vec<String> = (setup.editor.scene().0.iter())
        .take(2)
        .map(|row| row.text())
        .collect();
    assert_eq!(shown, ["..", "~ empty folder: o makes a file"]);
    // Until there's a name.
    typed(&mut setup, "onew<Esc>");
    setup.editor.settle(&setup.copy);
    let second = setup.editor.scene().0[1].text();
    assert_eq!(second, "new");
}
