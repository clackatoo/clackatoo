//! The finder driven by keys, with what the daemon finds given it as it
//! would come.

use clackatoo_protocol::{Done, FileLocation, Found, FoundBuffer, FoundLine, Side};
use winit::keyboard::{Key, KeyLocation, NamedKey};

use super::*;
use crate::input::Modifiers;

fn press(finder: &mut Finder, key: &Key, ctrl: bool) -> Outcome {
    let text = match key {
        Key::Character(text) if !ctrl => Some(text.to_string()),
        _ => None,
    };
    finder.key(&KeyPress {
        key,
        text: text.as_deref(),
        location: KeyLocation::Standard,
        code: None,
        modifiers: Modifiers {
            ctrl,
            ..Modifiers::default()
        },
    })
}

fn named(finder: &mut Finder, key: NamedKey) -> Outcome {
    press(finder, &Key::Named(key), false)
}

fn ctrl(finder: &mut Finder, letter: &str) -> Outcome {
    press(finder, &Key::Character(letter.into()), true)
}

fn type_text(finder: &mut Finder, text: &str) -> Outcome {
    let mut outcome = Outcome::Nothing;
    for ch in text.chars() {
        outcome = press(finder, &Key::Character(ch.to_string().into()), false);
    }
    outcome
}

/// The text of each row shown, in a list ten tall and wide.
fn shown(finder: &mut Finder) -> Vec<String> {
    let view = finder.view((10, 60), None);
    view.rows.into_iter().map(|row| row.text).collect()
}

/// The text picked out in the first row shown.
fn picked_out(finder: &mut Finder) -> String {
    let view = finder.view((10, 60), None);
    let row = &view.rows[0];
    let mut start = 0;
    let mut out = String::new();
    for &(end, ink) in &row.runs {
        if ink == Ink::Accent {
            out.push_str(&row.text[start..end]);
        }
        start = end;
    }
    out
}

fn files(finder: &mut Finder, paths: &[&str]) {
    finder.found(Found::Files(
        paths.iter().map(|&path| path.to_owned()).collect(),
    ));
}

fn root(finder: &mut Finder) {
    finder.found(Found::Root {
        path: "/home/jack/project".into(),
        name: "~/project".into(),
    });
}

#[test]
fn nothing_matching_says_so_once_its_sure() {
    let empty = |finder: &mut Finder| finder.view((10, 60), None).empty;
    let mut finder = Finder::new(Kind::Files, String::new(), Case::default());
    finder.searching(1);
    files(&mut finder, &["src/main.rs", "README"]);
    type_text(&mut finder, "zz");
    assert_eq!(empty(&mut finder), None, "still looking");
    finder.found(Found::Done(Done::All));
    assert_eq!(empty(&mut finder).as_deref(), Some("Nothing matches ‘zz’"));
    // Not with nothing typed, nor once something matches.
    ctrl(&mut finder, "u");
    assert_eq!(empty(&mut finder), None);
    type_text(&mut finder, "main");
    assert_eq!(empty(&mut finder), None);
    // A long pattern is cut short, in cells, a wide character two, to fit
    // the list: here 60 wide, or at most 40 of what's typed.
    ctrl(&mut finder, "u");
    type_text(&mut finder, &"x".repeat(50));
    let said = empty(&mut finder).expect("nothing matches");
    assert!(said.ends_with("x…’"), "{said}");
    ctrl(&mut finder, "u");
    type_text(&mut finder, &"字".repeat(30));
    let narrow = finder.view((10, 30), None).empty.expect("nothing matches");
    assert!(narrow.ends_with("字…’"), "{narrow}");
    assert!(crate::render::mono_width(&narrow) <= 30, "{narrow}");
    // Looking that failed says why in the count instead.
    let mut failed = Finder::new(Kind::Grep, "zz".into(), Case::default());
    failed.searching(2);
    failed.found(Found::Done(Done::Failed("no such place".into())));
    assert_eq!(empty(&mut failed), None);
    // And a list cut short of all there was may have missed a match.
    let mut cut = Finder::new(Kind::Files, "zz".into(), Case::default());
    cut.searching(3);
    files(&mut cut, &["README"]);
    cut.found(Found::Done(Done::CutShort));
    assert_eq!(empty(&mut cut), None);
}

#[test]
fn what_doesnt_fit_is_cut_short_by_cells() {
    assert_eq!(view::cut("abc", 3), "abc");
    assert_eq!(view::cut("abcd", 3), "ab…");
    assert_eq!(view::cut("字字字", 5), "字字…");
    assert_eq!(view::cut("字字字", 4), "字…");
    assert_eq!(view::cut("abc", 0), "");
}

#[test]
fn files_streaming_in_are_ranked_and_filtered_as_typed() {
    let mut finder = Finder::new(Kind::Files, String::new(), Case::default());
    finder.searching(1);
    root(&mut finder);
    files(&mut finder, &["src/main.rs", "README"]);
    let view = finder.view((10, 60), None);
    assert_eq!(view.title, "Files");
    assert_eq!(view.place.as_deref(), Some("~/project"));
    assert_eq!(view.count, "2 / 2…", "still looking");
    assert_eq!(
        shown(&mut finder),
        ["README", "src/main.rs"],
        "shortest first"
    );

    assert_eq!(type_text(&mut finder, "man"), Outcome::Changed);
    assert_eq!(shown(&mut finder), ["src/main.rs"]);
    assert_eq!(picked_out(&mut finder), "man");
    // A directory is quieter than a file's name.
    let view = finder.view((10, 60), None);
    assert_eq!(view.rows[0].runs[0], (4, Ink::Muted));
    // What comes later is matched as it comes.
    files(&mut finder, &["docs/manual.md", "lib.rs"]);
    finder.found(Found::Done(Done::All));
    assert_eq!(shown(&mut finder).len(), 2);
    assert_eq!(finder.view((10, 60), None).count, "2 / 4");
    assert_eq!(finder.search(), None);

    // Deleting widens it again; Ctrl+W takes a word, Ctrl+U the lot.
    named(&mut finder, NamedKey::Backspace);
    assert_eq!(finder.query(), "ma");
    type_text(&mut finder, " src");
    ctrl(&mut finder, "w");
    assert_eq!(finder.query(), "ma ");
    ctrl(&mut finder, "u");
    assert_eq!(shown(&mut finder).len(), 4);
    assert_eq!(named(&mut finder, NamedKey::Backspace), Outcome::Nothing);

    // What's typed to start with filters what comes.
    let mut started = Finder::new(Kind::Files, "man".into(), Case::default());
    started.searching(2);
    files(&mut started, &["src/main.rs", "README"]);
    assert_eq!(shown(&mut started), ["src/main.rs"]);

    // Cut short, and failed, it says so.
    finder.found(Found::Done(Done::CutShort));
    assert_eq!(finder.view((10, 60), None).count, "4 / 4+");
    finder.found(Found::Done(Done::Failed("no such place".into())));
    assert_eq!(finder.view((10, 60), None).count, "no such place");
}

#[test]
fn keys_move_through_the_list_and_open_what_is_picked_where_asked() {
    let mut finder = Finder::new(Kind::Files, String::new(), Case::default());
    finder.searching(1);
    root(&mut finder);
    files(&mut finder, &["a", "bb", "ccc", "dddd", "eeeee"]);
    let opening = |path: &str, place| {
        Outcome::Open(Opening {
            path: format!("/home/jack/project/{path}"),
            at: None,
            place,
        })
    };
    assert_eq!(
        named(&mut finder, NamedKey::Enter),
        opening("a", Place::Here)
    );
    named(&mut finder, NamedKey::ArrowDown);
    ctrl(&mut finder, "n");
    assert_eq!(
        ctrl(&mut finder, "v"),
        opening("ccc", Place::Beside(Side::Right))
    );
    ctrl(&mut finder, "p");
    assert_eq!(
        ctrl(&mut finder, "x"),
        opening("bb", Place::Beside(Side::Below))
    );
    named(&mut finder, NamedKey::ArrowUp);
    assert_eq!(
        named(&mut finder, NamedKey::ArrowUp),
        Outcome::Nothing,
        "at the top"
    );
    assert_eq!(ctrl(&mut finder, "t"), opening("a", Place::Tab));

    // The list scrolls to keep what's picked shown.
    let view = finder.view((2, 60), None);
    assert_eq!((view.rows.len(), view.selected), (2, Some(0)));
    named(&mut finder, NamedKey::PageDown);
    let view = finder.view((2, 60), None);
    let texts: Vec<&str> = view.rows.iter().map(|row| row.text.as_str()).collect();
    assert_eq!((texts, view.selected), (vec!["bb", "ccc"], Some(1)));
    named(&mut finder, NamedKey::PageDown);
    named(&mut finder, NamedKey::PageDown);
    assert_eq!(
        ctrl(&mut finder, "t"),
        opening("eeeee", Place::Tab),
        "at the end"
    );

    assert_eq!(named(&mut finder, NamedKey::Escape), Outcome::Close);
    assert_eq!(ctrl(&mut finder, "c"), Outcome::Close);
    // Ctrl+Shift+C is the terminal's, not the finder's.
    let shifted = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };
    let key = Key::Character("C".into());
    let press = KeyPress {
        key: &key,
        text: None,
        location: KeyLocation::Standard,
        code: None,
        modifiers: shifted,
    };
    assert_eq!(finder.key(&press), Outcome::Nothing);
    // A click picks a row, and a double click opens it; the wheel moves.
    finder.view((2, 60), None);
    assert_eq!(finder.click(1, false), Outcome::Changed);
    assert_eq!(finder.click(5, true), Outcome::Nothing, "no row there");
    assert_eq!(finder.scroll(-1), Outcome::Changed);
    assert_eq!(finder.click(0, true), opening("dddd", Place::Here));
    // Nothing to pick opens nothing.
    type_text(&mut finder, "zzz");
    assert_eq!(named(&mut finder, NamedKey::Enter), Outcome::NothingPicked);
}

#[test]
fn a_grep_searches_as_typed_and_opens_at_the_line() {
    let mut finder = Finder::new(Kind::Grep, String::new(), Case::default());
    assert_eq!(type_text(&mut finder, "fn"), Outcome::Search);
    finder.searching(2);
    root(&mut finder);
    finder.found(Found::Lines(vec![FoundLine {
        path: "src/a.rs".into(),
        line: 2,
        col: 5,
        text: "\t    fn main() {}".into(),
        matches: vec![(5, 7)],
    }]));
    // Its indent left out, its match picked out, and its file quieter.
    assert_eq!(shown(&mut finder), ["src/a.rs:3: fn main() {}"]);
    assert_eq!(picked_out(&mut finder), "fn");
    let view = finder.view((10, 60), None);
    assert_eq!(view.rows[0].runs[0], ("src/a.rs:3: ".len(), Ink::Muted));
    assert_eq!(
        named(&mut finder, NamedKey::Enter),
        Outcome::Open(Opening {
            path: "/home/jack/project/src/a.rs".into(),
            at: Some((2, 5)),
            place: Place::Here,
        })
    );
    // A new search starts the list again.
    assert_eq!(type_text(&mut finder, " "), Outcome::Search);
    finder.searching(3);
    assert!(shown(&mut finder).is_empty());
    finder.not_searching();
    assert_eq!(finder.view((10, 60), None).count, "0 / 0");
}

#[test]
fn buffers_say_which_have_changes() {
    let mut finder = Finder::new(Kind::Buffers, String::new(), Case::default());
    finder.searching(1);
    root(&mut finder);
    finder.found(Found::Buffers(vec![
        FoundBuffer {
            path: "src/a.rs".into(),
            modified: true,
        },
        FoundBuffer {
            path: "/etc/hosts".into(),
            modified: false,
        },
    ]));
    assert_eq!(shown(&mut finder), ["/etc/hosts", "src/a.rs [+]"]);
    type_text(&mut finder, "a.rs");
    assert_eq!(
        named(&mut finder, NamedKey::Enter),
        Outcome::Open(Opening {
            path: "/home/jack/project/src/a.rs".into(),
            at: None,
            place: Place::Here,
        })
    );
}

fn place(path: &str, line: u64, text: &str) -> FileLocation {
    FileLocation {
        path: path.into(),
        line,
        col: 4,
        offset: None,
        text: text.into(),
    }
}

#[test]
fn places_are_listed_from_the_directory_they_share_and_filtered() {
    let found = [
        place("/p/crates/gui/src/a.rs", 0, "impl A for X"),
        place("/p/crates/daemon/src/b.rs", 9, "impl A for Y"),
    ];
    let mut finder = Finder::places(
        Kind::Places,
        &found,
        "/p/crates/gui/src/main.rs",
        Case::default(),
    );
    assert_eq!(
        shown(&mut finder),
        [
            "gui/src/a.rs:1: impl A for X",
            "daemon/src/b.rs:10: impl A for Y"
        ]
    );
    assert_eq!(
        finder.view((10, 60), None).place.as_deref(),
        Some("/p/crates")
    );
    type_text(&mut finder, "Y");
    assert_eq!(
        named(&mut finder, NamedKey::Enter),
        Outcome::Open(Opening {
            path: "/p/crates/daemon/src/b.rs".into(),
            at: Some((9, 4)),
            place: Place::Here,
        })
    );
    // Sharing no directory, they're whole.
    let found = [place("/a/x", 0, ""), place("/b/y", 0, "")];
    let mut finder = Finder::places(Kind::Places, &found, "/a/z", Case::default());
    assert_eq!(shown(&mut finder), ["/a/x:1: ", "/b/y:1: "]);
}

#[test]
fn the_preview_follows_what_is_picked() {
    let found = [
        place("/p/a.rs", 50, "one"),
        place("/p/a.rs", 60, "two"),
        place("/p/a.rs", 200, "far"),
        place("/p/b.rs", 0, "other"),
    ];
    let mut finder = Finder::places(Kind::Places, &found, "/p/main.rs", Case::default());
    assert_eq!(finder.preview_wanted(), Some(("/p/a.rs".into(), 50)));
    finder.previewing(7, "/p/a.rs".into(), 50);
    assert_eq!(finder.preview_search(), Some(7));
    assert_eq!(finder.preview_wanted(), None, "asked for already");
    let lines: Vec<String> = (0..100).map(|n| format!("line {}", n + 30)).collect();
    finder.preview_found(Found::Preview { first: 30, lines });
    finder.preview_found(Found::Done(Done::All));
    assert_eq!(finder.preview_search(), None);
    let preview = finder.view((10, 60), Some((9, 40))).preview.unwrap();
    assert_eq!(preview.at, Some(3));
    assert_eq!(preview.lines[3].text, " 51 line 50");

    // A place near the one shown needs nothing more; one far off does.
    named(&mut finder, NamedKey::ArrowDown);
    assert_eq!(finder.preview_wanted(), None);
    let preview = finder.view((10, 60), Some((9, 40))).preview.unwrap();
    assert_eq!(preview.lines[preview.at.unwrap()].text, " 61 line 60");
    named(&mut finder, NamedKey::ArrowDown);
    assert_eq!(finder.preview_wanted(), Some(("/p/a.rs".into(), 200)));
    named(&mut finder, NamedKey::ArrowDown);
    assert_eq!(finder.preview_wanted(), Some(("/p/b.rs".into(), 0)));
    assert!(
        finder.view((10, 60), Some((9, 40))).preview.is_none(),
        "not come"
    );
    finder.previewing(8, "/p/b.rs".into(), 0);
    finder.preview_found(Found::Done(Done::Failed("it isn't text".into())));
    let preview = finder.view((10, 60), Some((9, 40))).preview.unwrap();
    assert_eq!(preview.lines[0].text, "it isn't text");
}

#[test]
fn a_symbols_name_is_plain_and_what_it_is_quieter_indented_as_deep_as_it_is() {
    let symbol = |name: &str, kind, depth, container: &str| Symbol {
        name: name.into(),
        kind,
        depth,
        container: container.into(),
        line: 2,
        col: 4,
        offset: Some(20),
    };
    let mut finder = Finder::new(Kind::Symbols, String::new(), Case::default());
    finder.ask(3);
    assert_eq!(finder.view((10, 60), None).count, "0 / 0…");
    let symbols = vec![
        symbol("Engine", 23, 0, ""),
        symbol("start", 99, 1, "Engine"),
    ];
    // More than the daemon passed on: the count says so.
    finder.answered(
        "/p/a.rs",
        Answer::Symbols {
            symbols,
            more: true,
        },
    );
    assert_eq!(finder.asking(), None);
    assert_eq!(finder.view((10, 60), None).count, "2 / 2+");
    assert_eq!(
        shown(&mut finder),
        ["Engine  struct", "  start  symbol · Engine"]
    );
    let view = finder.view((10, 60), None);
    let start = &view.rows[1];
    assert_eq!(
        start.runs,
        [
            ("  start".len(), Ink::Plain),
            (start.text.len(), Ink::Muted)
        ]
    );
    // What's typed is picked out, and the place is the file's, absolute.
    type_text(&mut finder, "st");
    assert_eq!(picked_out(&mut finder), "st");
    assert_eq!(
        named(&mut finder, NamedKey::Enter),
        Outcome::Open(Opening {
            path: "/p/a.rs".into(),
            at: Some((2, 4)),
            place: Place::Here,
        })
    );
}
