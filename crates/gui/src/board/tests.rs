use clackatoo_protocol::TerminalId;

use super::*;

fn summary(entries: u32, archived: bool) -> BoardSummary {
    BoardSummary {
        id: BoardId(1),
        name: "plan".into(),
        purpose: "the rewrite".into(),
        scope: BoardScope::Workspace("w".into()),
        archived,
        created: 0,
        entries,
    }
}

fn entry(id: u32, kind: PostKind, text: &str) -> BoardEntry {
    BoardEntry {
        id,
        kind,
        by: BoardAuthor::Chat {
            chat: TerminalId(4),
            agent: "Codex".into(),
            title: "t".into(),
        },
        at: now(),
        text: text.into(),
        links: Vec::new(),
        subject: None,
        expires: None,
        released: None,
        edits: Vec::new(),
    }
}

/// A pane 40 by 10 showing `entries`.
fn pane(entries: Vec<BoardEntry>) -> BoardPane {
    let options = Options {
        cursorline: true,
        ..Options::default()
    };
    let mut pane = BoardPane::new(BoardId(1), &options);
    pane.resize(Size::new(40, 10).unwrap());
    let count = u32::try_from(entries.len()).unwrap();
    pane.update(BoardUpdate::Snapshot {
        summary: summary(count, false),
        entries,
    });
    pane.settle();
    pane
}

/// The text of the rows drawn, trimmed.
fn text(pane: &BoardPane) -> Vec<String> {
    let (rows, _) = pane.scene();
    rows.iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

fn typed(pane: &mut BoardPane, keys: &str) {
    for ch in keys.chars() {
        pane.stroke(Stroke::char(ch));
    }
}

#[test]
fn an_empty_board_says_how_to_post_under_its_heading_and_its_title_is_capitalised() {
    let pane = pane(Vec::new());
    assert_eq!(
        text(&pane)[..2],
        [
            "plan  workspace w · 0 entries · the rewr",
            "Nothing here yet: i posts a note.",
        ],
        "lined up with the heading"
    );
    assert_eq!(pane.title(), "Board: plan");
}

#[test]
fn entries_show_who_wrote_them_and_wrap_under_their_heading() {
    let mut claim = entry(2, PostKind::Claim, "rewriting it");
    claim.subject = Some("the lexer".into());
    claim.expires = Some(now() + 3_600_000 + 120_000);
    let mut note = entry(
        1,
        PostKind::Note,
        "use logos for the lexer, it is much faster than ours",
    );
    note.links = vec![BoardLink::File {
        path: "/p/a.rs".into(),
        line: Some(3),
    }];
    let pane = pane(vec![note, claim]);
    assert_eq!(
        text(&pane),
        [
            "plan  workspace w · 2 entries · the rewr",
            "#1 note  chat 4 · Codex · now",
            "   use logos for the lexer, it is much",
            "   faster than ours",
            "   → /p/a.rs:3",
            "",
            "#2 claim  chat 4 · Codex · now  held, 1h",
            "   the lexer",
            "   rewriting it",
            "",
        ]
    );
    // The cursor follows the newest, and its whole entry is lit.
    let (_, cursor) = pane.scene();
    assert_eq!(cursor.map(|cursor| (cursor.line, cursor.col)), Some((6, 0)));
    let lit: Vec<usize> = pane.highlights().iter().map(|lit| lit.line).collect();
    assert_eq!(lit, [6, 7, 8]);
}

#[test]
fn posting_editing_releasing_and_archiving_go_to_the_daemon() {
    let mut claim = entry(2, PostKind::Claim, "");
    claim.subject = Some("x".into());
    claim.expires = Some(now() + 60_000);
    let mut pane = pane(vec![entry(1, PostKind::Note, "first"), claim]);
    typed(&mut pane, "i");
    assert!(pane.prompting());
    typed(&mut pane, "done it");
    assert_eq!(pane.label(), "post a note: done it▏");
    pane.stroke(Stroke::Enter);
    typed(&mut pane, "rgreen");
    pane.stroke(Stroke::Enter);
    // An empty one goes nowhere.
    typed(&mut pane, "i");
    pane.stroke(Stroke::Enter);
    typed(&mut pane, "x");
    typed(&mut pane, "ke");
    for _ in 0.."first".len() {
        pane.stroke(Stroke::Backspace);
    }
    typed(&mut pane, "1st");
    pane.stroke(Stroke::Enter);
    typed(&mut pane, "A");
    assert_eq!(
        pane.take_outbox(),
        [
            BoardAction::Post {
                kind: PostKind::Note,
                text: "done it".into(),
            },
            BoardAction::Post {
                kind: PostKind::Result,
                text: "green".into(),
            },
            BoardAction::Release { entry: 2 },
            BoardAction::Edit {
                entry: 1,
                text: "1st".into(),
            },
            BoardAction::Archive(true),
        ]
    );
    // Not a claim, nothing to release.
    typed(&mut pane, "x");
    assert_eq!(pane.label(), "there's no claim held here");
    assert!(pane.take_outbox().is_empty());
    typed(&mut pane, "q");
    assert_eq!(pane.take_asks(), [Ask::Close]);
}

#[test]
fn changes_land_in_place_and_an_archived_board_takes_no_posts() {
    let mut pane = pane(vec![entry(1, PostKind::Note, "first")]);
    let mut edited = entry(1, PostKind::Note, "changed");
    edited.edits = vec![(0, BoardAuthor::User, "first".into())];
    pane.update(BoardUpdate::Changed {
        summary: summary(2, false),
        entries: vec![edited, entry(2, PostKind::Result, "second")],
    });
    pane.settle();
    let shown = text(&pane);
    assert!(shown[1].ends_with("now · edited"), "{shown:?}");
    assert_eq!(shown[2], "   changed");
    assert_eq!(shown[5], "   second");
    pane.update(BoardUpdate::Refused("no room".into()));
    assert_eq!(pane.label(), "no room");
    pane.update(BoardUpdate::Changed {
        summary: summary(2, true),
        entries: Vec::new(),
    });
    typed(&mut pane, "i");
    assert!(!pane.prompting());
    assert_eq!(pane.label(), "the board is archived: A unarchives it");
    typed(&mut pane, "A");
    assert_eq!(pane.take_outbox(), [BoardAction::Archive(false)]);
}

#[test]
fn the_cursor_moves_by_entries_and_stays_where_it_was_put() {
    let entries = (1..=6)
        .map(|id| entry(id, PostKind::Note, &format!("entry {id}")))
        .collect();
    let mut pane = pane(entries);
    typed(&mut pane, "gg");
    pane.settle();
    assert_eq!(text(&pane)[1], "#1 note  chat 4 · Codex · now");
    typed(&mut pane, "2j");
    pane.settle();
    let (_, cursor) = pane.scene();
    assert_eq!(cursor.unwrap().line, 7, "on entry 3");
    // A new entry doesn't move a cursor put somewhere.
    pane.update(BoardUpdate::Changed {
        summary: summary(7, false),
        entries: vec![entry(7, PostKind::Note, "seven")],
    });
    pane.settle();
    assert_eq!(pane.scene().1.unwrap().line, 7);
    // At the newest, it follows the next.
    typed(&mut pane, "G");
    pane.update(BoardUpdate::Changed {
        summary: summary(8, false),
        entries: vec![entry(8, PostKind::Note, "eight")],
    });
    pane.settle();
    let shown = text(&pane);
    assert!(shown.iter().any(|line| line == "   eight"), "{shown:?}");
    // A click on an entry's heading puts the cursor there; between
    // entries, nowhere.
    let heading = shown.iter().position(|line| line.starts_with('#')).unwrap();
    assert!(pane.click(heading));
    pane.settle();
    assert_eq!(pane.scene().1.unwrap().line, heading);
    let gap = shown.iter().position(String::is_empty).unwrap();
    assert!(!pane.click(gap));
}

#[test]
fn text_wraps_at_spaces_and_cuts_what_is_too_long() {
    assert_eq!(wrap("a bb ccc", 4), ["a bb", "ccc"]);
    assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
    assert_eq!(wrap("", 3), [""]);
    assert_eq!(span(90_000), "1m");
    assert_eq!(span(3_600_000 + 120_000), "1h 2m");
    assert_eq!(span(3 * 86_400_000), "3d");
}

/// A pane of ten entries, each two lines and a gap, three to a view.
fn ten() -> BoardPane {
    let entries = (1..=10)
        .map(|id| entry(id, PostKind::Note, &format!("entry {id}")))
        .collect();
    let mut pane = pane(entries);
    keys(&mut pane, "gg");
    pane.settle();
    pane
}

fn keys(pane: &mut BoardPane, keys: &str) {
    for stroke in Stroke::read(keys) {
        pane.stroke(stroke);
    }
}

#[test]
fn a_board_moves_by_entries_as_every_list_does() {
    let mut pane = ten();
    assert_eq!(ListRows::shown(&pane), 0..3);
    keys(&mut pane, "L");
    assert_eq!(pane.cursor, 2);
    keys(&mut pane, "M");
    assert_eq!(pane.cursor, 1);
    keys(&mut pane, "5G");
    assert_eq!(pane.cursor, 4);
    pane.settle();
    keys(&mut pane, "zt");
    pane.settle();
    assert_eq!(ListRows::top(&pane), 4);
    keys(&mut pane, "<C-y>");
    pane.settle();
    assert_eq!((ListRows::top(&pane), pane.cursor), (3, 4));
    // Ctrl+D: half the entries in view, the cursor going with them.
    keys(&mut pane, "<C-d>");
    pane.settle();
    assert_eq!((ListRows::top(&pane), pane.cursor), (4, 5));
    // Ctrl+F: a page, as far as in any pane: a view of three keeps none.
    keys(&mut pane, "<C-f>");
    pane.settle();
    assert_eq!((ListRows::top(&pane), pane.cursor), (7, 7));
    keys(&mut pane, "<End>");
    assert_eq!(pane.cursor, 9);
    keys(&mut pane, "3<C-p>");
    assert_eq!(pane.cursor, 6);
    // What's half typed shows in the bar.
    keys(&mut pane, "2g");
    assert!(pane.label().starts_with("2g  plan"), "{}", pane.label());
    keys(&mut pane, "g");
    assert_eq!(pane.cursor, 1);
}

#[test]
fn searches_find_entries_by_what_they_say() {
    let mut pane = ten();
    keys(&mut pane, "/entry 7");
    assert!(pane.prompting());
    assert_eq!(pane.label(), "/entry 7▏");
    keys(&mut pane, "<CR>");
    assert!(!pane.prompting());
    assert_eq!(pane.cursor, 6);
    keys(&mut pane, "/entry 1<CR>");
    assert_eq!(pane.cursor, 9, "entry 10 next after 7");
    keys(&mut pane, "n");
    assert_eq!(pane.cursor, 0, "round to entry 1");
    keys(&mut pane, "N");
    assert_eq!(pane.cursor, 9);
    keys(&mut pane, "/nothing<CR>");
    assert_eq!(pane.label(), "no entry matches nothing");
}

#[test]
fn a_boards_keys_help_comes_from_how_it_reads_them() {
    let pane = ten();
    let rows = pane.shortcut_context().rows;
    let has = |key: &str, said: &str| rows.iter().any(|(k, d)| k == key && d == said);
    assert!(has("i", "Post a note"), "{rows:?}");
    assert!(has("r", "Post a result"));
    assert!(has("zz", "Cursor line at centre"));
    assert!(has("n", "Repeat search"));
    assert!(!rows.iter().any(|(key, _)| key == "p"), "nothing to put");
}

#[test]
fn a_paste_goes_into_what_is_typed_in_the_bar() {
    let mut pane = ten();
    assert!(!pane.paste("x"), "nothing typed");
    keys(&mut pane, "/entry");
    assert!(pane.paste(" 7\n"));
    assert_eq!(pane.label(), "/entry 7 ▏");
    keys(&mut pane, "<Esc>i");
    assert!(pane.paste("a\tb"));
    assert_eq!(pane.label(), "post a note: a b▏");
}

#[test]
fn a_key_that_does_nothing_still_clears_a_message_and_redraws() {
    let mut pane = ten();
    keys(&mut pane, "/nothing<CR>");
    assert_eq!(pane.label(), "no entry matches nothing");
    assert!(pane.stroke(Stroke::Escape), "drawn again");
    assert_ne!(pane.label(), "no entry matches nothing");
}
