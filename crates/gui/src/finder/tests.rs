//! The finder driven by keys, with what the daemon finds given it as it
//! would come.

use clackatoo_protocol::{Done, FileLocation, Found, FoundBuffer, FoundLine, FoundSymbol, Side};
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
        if ink == Ink::Matched {
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
    assert_eq!(view::cut("abc", 3, false), "abc");
    assert_eq!(view::cut("abcd", 3, false), "ab…");
    assert_eq!(view::cut("字字字", 5, false), "字字…");
    assert_eq!(view::cut("字字字", 4, false), "字…");
    assert_eq!(view::cut("abc", 0, false), "");
    // From the start, keeping the end, as a long path is.
    assert_eq!(view::cut("~/a/b/c", 5, true), "…/b/c");
    assert_eq!(view::cut("字字字", 5, true), "…字字");
    assert_eq!(view::cut("abc", 3, true), "abc");
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
    finder.preview_found(Found::Preview {
        first: 30,
        lines,
        spans: Vec::new(),
    });
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

/// A row's runs, as text and ink.
fn inked(row: &Row) -> Vec<(&str, Ink)> {
    let mut start = 0;
    (row.runs.iter())
        .map(|&(end, ink)| {
            let text = &row.text[start..end];
            start = end;
            (text, ink)
        })
        .collect()
}

#[test]
fn every_list_picks_out_the_letters_typed_as_files_does() {
    // Buffers, a language server's places, and lessons.
    let mut finder = Finder::new(Kind::Buffers, String::new(), Case::default());
    finder.searching(1);
    finder.found(Found::Buffers(vec![FoundBuffer {
        path: "src/area.rs".into(),
        modified: false,
    }]));
    type_text(&mut finder, "area");
    assert_eq!(picked_out(&mut finder), "area");
    let found = [place("/p/a.rs", 1, "let area = 2;")];
    let mut finder = Finder::places(Kind::Places, &found, "/p/main.rs", Case::default());
    type_text(&mut finder, "area");
    assert_eq!(picked_out(&mut finder), "area");
    let mut finder = Finder::local(
        Kind::Lessons,
        vec![LocalItem {
            name: "basics".into(),
            text: "basics  The basics".into(),
            name_len: 6,
            preview: Vec::new(),
            alert: None,
        }],
        Case::default(),
    );
    type_text(&mut finder, "basics");
    assert_eq!(picked_out(&mut finder), "basics");
}

#[test]
fn a_projects_symbols_have_the_letters_searched_for_picked_out_in_their_names() {
    let symbol = |name: &str| FoundSymbol {
        path: "src/area.rs".into(),
        symbol: Symbol {
            name: name.into(),
            kind: 12,
            depth: 0,
            container: String::new(),
            line: 0,
            col: 0,
            offset: None,
        },
    };
    let mut finder = Finder::new(Kind::ProjectSymbols, String::new(), Case::default());
    assert_eq!(type_text(&mut finder, "ar"), Outcome::Search);
    finder.searching(2);
    root(&mut finder);
    // The server matches as it likes: none is left out, nor reordered.
    finder.found(Found::Symbols(vec![symbol("parse"), symbol("Circle")]));
    assert_eq!(
        shown(&mut finder),
        [
            "parse  function  src/area.rs:1",
            "Circle  function  src/area.rs:1"
        ]
    );
    // In the name only, not the path, which the server didn't match.
    assert_eq!(picked_out(&mut finder), "ar");
    let view = finder.view((10, 60), None);
    assert!(
        inked(&view.rows[1])
            .iter()
            .all(|&(_, ink)| ink != Ink::Matched)
    );
    // Typed but not yet searched for: what was searched for stays lit.
    assert_eq!(type_text(&mut finder, "s"), Outcome::Search);
    assert_eq!(picked_out(&mut finder), "ar");
    finder.searching(3);
    finder.found(Found::Symbols(vec![symbol("parse")]));
    assert_eq!(picked_out(&mut finder), "ars");
}

#[test]
fn whats_wrong_is_coloured_by_how_much_it_matters() {
    let mut finder = Finder::new(Kind::Diagnostics, String::new(), Case::default());
    finder.searching(1);
    let line = |text: &str| FoundLine {
        path: "a.fk".into(),
        line: 2,
        col: 0,
        text: text.into(),
        matches: Vec::new(),
    };
    finder.found(Found::Lines(vec![
        line("error: bad is bad [7 fake]"),
        line("hint: errors: none"),
        line("odd: not ours"),
    ]));
    let view = finder.view((10, 60), None);
    let red = Ink::Color(crate::colors::diagnostic(clackatoo_text::Severity::Error));
    let cyan = Ink::Color(crate::colors::diagnostic(clackatoo_text::Severity::Hint));
    assert_eq!(
        inked(&view.rows[0]),
        [
            ("a.fk:3: ", Ink::Muted),
            ("error", red),
            (": bad is bad [7 fake]", Ink::Plain)
        ]
    );
    assert_eq!(inked(&view.rows[1])[1], ("hint", cyan));
    assert_eq!(inked(&view.rows[2])[1], ("odd: not ours", Ink::Plain));
    // What's typed is picked out over it.
    type_text(&mut finder, "err");
    let view = finder.view((10, 60), None);
    assert_eq!(inked(&view.rows[0])[1], ("err", Ink::Matched));
    assert_eq!(inked(&view.rows[0])[2], ("or", red));
}

#[test]
fn a_preview_is_coloured_as_its_buffer_and_a_greps_matches_picked_out_in_its_line() {
    let mut finder = Finder::new(Kind::Grep, String::new(), Case::default());
    type_text(&mut finder, "finder");
    finder.searching(2);
    root(&mut finder);
    // The line's text from some way in, as a long one comes: its matches
    // are at bytes 8 and 17 of the line.
    finder.found(Found::Lines(vec![FoundLine {
        path: "a.rs".into(),
        line: 2,
        col: 8,
        text: "t finder = finder();".into(),
        matches: vec![(2, 8), (11, 17)],
    }]));
    let path = "/home/jack/project/a.rs";
    assert_eq!(finder.preview_wanted(), Some((path.into(), 2)));
    finder.previewing(3, path.into(), 2);
    let lines = ["// one", "fn two() {", "    let finder = finder();", "}"];
    let span = |start, end, kind| Span { start, end, kind };
    finder.preview_found(Found::Preview {
        first: 0,
        lines: lines.map(str::to_owned).to_vec(),
        // A comment, and `let`, as bytes of the lines with their breaks.
        spans: vec![
            span(0, 6, clackatoo_text::Kind::Comment),
            span(22, 25, clackatoo_text::Kind::Keyword),
        ],
    });
    let preview = finder.view((10, 60), Some((9, 40))).preview.unwrap();
    assert_eq!(preview.at, Some(2));
    let comment = crate::colors::syntax_ink(clackatoo_text::Kind::Comment);
    let keyword = crate::colors::syntax_ink(clackatoo_text::Kind::Keyword);
    assert_eq!(
        inked(&preview.lines[0]),
        [("1 ", Ink::Muted), ("// one", comment)]
    );
    assert_eq!(
        inked(&preview.lines[2]),
        [
            ("3 ", Ink::Muted),
            ("    ", Ink::Plain),
            ("let", keyword),
            (" ", Ink::Plain),
            ("finder", Ink::Matched),
            (" = ", Ink::Plain),
            ("finder", Ink::Matched),
            ("();", Ink::Plain),
        ]
    );
    // Only the place's line has them picked out.
    assert_eq!(
        inked(&preview.lines[3]),
        [("4 ", Ink::Muted), ("}", Ink::Plain)]
    );
}

#[test]
fn an_indented_greps_match_is_picked_out_in_a_preview_coloured_by_bytes() {
    let mut finder = Finder::new(Kind::Grep, String::new(), Case::default());
    type_text(&mut finder, "x*");
    finder.searching(2);
    root(&mut finder);
    // As the daemon sends `x*` on "\té x": its place is its first match
    // that isn't empty, at byte 4.
    finder.found(Found::Lines(vec![FoundLine {
        path: "a.rs".into(),
        line: 0,
        col: 4,
        text: "\té x".into(),
        matches: vec![(4, 5)],
    }]));
    assert_eq!(shown(&mut finder), ["a.rs:1: é x"]);
    let path = "/home/jack/project/a.rs";
    finder.previewing(3, path.into(), 0);
    // `é` coloured, by its two bytes.
    let span = Span {
        start: 1,
        end: 3,
        kind: clackatoo_text::Kind::String,
    };
    finder.preview_found(Found::Preview {
        first: 0,
        lines: vec!["\té x".into()],
        spans: vec![span],
    });
    let preview = finder.view((10, 60), Some((9, 40))).preview.unwrap();
    assert_eq!(
        inked(&preview.lines[0]),
        [
            ("1 ", Ink::Muted),
            ("    ", Ink::Plain),
            ("é", crate::colors::syntax_ink(clackatoo_text::Kind::String)),
            (" ", Ink::Plain),
            ("x", Ink::Matched),
        ]
    );
}

/// How a keyword is coloured.
fn keyword_ink() -> Ink {
    crate::colors::syntax_ink(clackatoo_text::Kind::Keyword)
}

/// The preview of `path`, `lines` as they come from the daemon's search
/// numbered `search`, with no colours, as a file not open comes.
fn plain_preview(finder: &mut Finder, search: u64, path: &str, lines: &[&str]) {
    finder.previewing(search, path.into(), 0);
    finder.preview_found(Found::Preview {
        first: 0,
        lines: lines.iter().map(|&line| line.to_owned()).collect(),
        spans: Vec::new(),
    });
}

#[test]
fn a_preview_of_a_file_not_open_is_asked_to_be_coloured_once_the_latest_only() {
    let now = Instant::now();
    let mut finder = Finder::new(Kind::Files, String::new(), Case::default());
    finder.searching(1);
    root(&mut finder);
    // b.rs is picked, as the list shows only it.
    files(&mut finder, &["b.rs"]);
    plain_preview(
        &mut finder,
        2,
        "/home/jack/project/a.rs",
        &["fn a() {}", "x"],
    );
    // Its code, in its file's language, as the daemon numbers its bytes:
    // each line with its break.
    let (search, code) = finder.take_colour_ask(now).expect("an ask");
    assert_eq!(search, 2);
    assert_eq!(code.language, "rs");
    assert_eq!(code.text, "fn a() {}\nx\n");
    assert_eq!(finder.take_colour_ask(now), None, "once");
    finder.colours_asked(Some(7), (search, code), now);
    assert_eq!(finder.colour_ask(), Some(7));
    // Another preview comes while that's answered: it waits its turn.
    plain_preview(&mut finder, 3, "/home/jack/project/b.rs", &["let b = 1;"]);
    assert_eq!(finder.take_colour_ask(now), None, "one at a time");
    // The answer for the preview gone is let go...
    let keyword = |start, end| Span {
        start,
        end,
        kind: clackatoo_text::Kind::Keyword,
    };
    // ...though it's drawn again, for the one shown to be asked about
    // after...
    assert!(finder.coloured(vec![keyword(0, 2)]));
    assert_eq!(finder.colour_ask(), None);
    let shown = finder.view((10, 60), Some((9, 40))).preview.unwrap();
    assert!(
        inked(&shown.lines[0])
            .iter()
            .all(|&(_, ink)| ink != keyword_ink()),
        "its colours aren't the other's"
    );
    // ...and then it is.
    let (search, code) = finder.take_colour_ask(now).expect("the latest");
    assert_eq!((search, code.text.as_str()), (3, "let b = 1;\n"));
    finder.colours_asked(Some(8), (search, code), now);
    // Its answer colours it, out of order as it may come.
    assert!(finder.coloured(vec![keyword(8, 9), keyword(0, 3)]));
    let preview = finder.view((10, 60), Some((9, 40))).preview.unwrap();
    let ink = keyword_ink();
    assert_eq!(
        inked(&preview.lines[0]),
        [
            ("1 ", Ink::Muted),
            ("let", ink),
            (" b = ", Ink::Plain),
            ("1", ink),
            (";", Ink::Plain)
        ]
    );
    assert!(!finder.coloured(vec![keyword(0, 3)]), "answered already");
}

#[test]
fn a_preview_already_coloured_or_of_no_language_or_lost_with_the_daemon_is_handled() {
    let now = Instant::now();
    let mut finder = Finder::new(Kind::Files, String::new(), Case::default());
    finder.searching(1);
    root(&mut finder);
    // A file with no ending names no language: nothing's asked.
    plain_preview(&mut finder, 2, "/home/jack/project/Makefile", &["all:"]);
    assert_eq!(finder.take_colour_ask(now), None);
    // Nor is an empty file.
    plain_preview(&mut finder, 3, "/home/jack/project/e.rs", &[]);
    assert_eq!(finder.take_colour_ask(now), None);
    // Nor a file open, which comes coloured.
    finder.previewing(4, "/home/jack/project/a.rs".into(), 0);
    finder.preview_found(Found::Preview {
        first: 0,
        lines: vec!["fn a".into()],
        spans: vec![Span {
            start: 0,
            end: 2,
            kind: clackatoo_text::Kind::Keyword,
        }],
    });
    assert_eq!(finder.take_colour_ask(now), None);
    // A daemon that can't colour code: it stays plain, with nothing
    // waiting.
    plain_preview(&mut finder, 5, "/home/jack/project/b.rs", &["b"]);
    let ask = finder.take_colour_ask(now).unwrap();
    finder.colours_asked(None, ask, now);
    assert_eq!(finder.colour_ask(), None);
    assert_eq!(finder.take_colour_ask(now), None);
    // The connection gone while it's answered: it's asked again.
    plain_preview(&mut finder, 6, "/home/jack/project/c.rs", &["c"]);
    let ask = finder.take_colour_ask(now).unwrap();
    finder.colours_asked(Some(9), ask, now);
    finder.forget_colour_ask();
    assert_eq!(finder.colour_ask(), None);
    let (search, code) = finder.take_colour_ask(now).expect("asked again");
    assert_eq!((search, code.text.as_str()), (6, "c\n"));
}

#[test]
fn an_ask_the_daemon_dropped_is_given_up_on_and_a_coloured_preview_wants_nothing() {
    let now = Instant::now();
    let mut finder = Finder::new(Kind::Files, String::new(), Case::default());
    finder.searching(1);
    root(&mut finder);
    plain_preview(&mut finder, 2, "/home/jack/project/a.rs", &["a"]);
    let ask = finder.take_colour_ask(now).unwrap();
    finder.colours_asked(Some(7), ask, now);
    // Its answer never comes, as a daemon with too many waiting drops it.
    plain_preview(&mut finder, 3, "/home/jack/project/b.rs", &["b"]);
    let soon = now + Duration::from_secs(1);
    assert_eq!(
        finder.take_colour_ask(soon),
        None,
        "still answered, for all it knows"
    );
    let later = now + COLOURING_GIVEN_UP;
    let (search, _) = finder.take_colour_ask(later).expect("given up on");
    assert_eq!(search, 3);
    // A preview that comes coloured, as an open file's does, leaves the
    // plain one before it unwanted.
    let code = Code {
        language: "rs".into(),
        text: "b\n".into(),
    };
    finder.colours_asked(Some(8), (3, code), later);
    plain_preview(&mut finder, 4, "/home/jack/project/c.rs", &["c"]);
    finder.previewing(5, "/home/jack/project/a.rs".into(), 0);
    finder.preview_found(Found::Preview {
        first: 0,
        lines: vec!["a".into()],
        spans: vec![Span {
            start: 0,
            end: 1,
            kind: clackatoo_text::Kind::Keyword,
        }],
    });
    assert!(!finder.coloured(Vec::new()), "nothing waits");
    assert_eq!(finder.take_colour_ask(later), None);
}

#[test]
fn an_empty_file_says_so_in_the_preview() {
    let mut finder = Finder::new(Kind::Files, String::new(), Case::default());
    finder.searching(1);
    root(&mut finder);
    files(&mut finder, &["empty.txt", "blank.txt"]);
    let preview = |finder: &mut Finder, lines: &[&str]| {
        let (path, line) = finder.preview_wanted().unwrap();
        finder.previewing(2, path, line);
        finder.preview_found(Found::Preview {
            first: 0,
            lines: lines.iter().map(|&line| line.to_owned()).collect(),
            spans: Vec::new(),
        });
        let view = finder.view((10, 60), Some((9, 40)));
        view.preview.unwrap().lines[0].clone()
    };
    let empty = preview(&mut finder, &[]);
    assert_eq!(inked(&empty), [("Empty file", Ink::Muted)]);
    // A file of one empty line isn't empty.
    named(&mut finder, NamedKey::ArrowDown);
    assert_eq!(preview(&mut finder, &[""]).text, "1 ");
}
