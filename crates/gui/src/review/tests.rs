//! A review pane driven by keys, with lists as the daemon sends them.

use clackatoo_protocol::{Made, ReviewComment};

use super::*;
use crate::vim::KeyState;

const CHAT: TerminalId = TerminalId(4);

/// A hunk at line `line`, from `base` to `text`, made by the chat's agent
/// in turn `turn` as agent hunk `id`.
fn hunk(line: u32, (base, text): (&str, &str), id: u64, turn: u32) -> ReviewHunk {
    ReviewHunk {
        line,
        before: vec!["fn main() {".to_owned()],
        base: base.to_owned(),
        text: text.to_owned(),
        after: vec!["}".to_owned()],
        agents: vec![id],
        by: Some(Made { chat: CHAT, turn }),
        reviewed: false,
    }
}

/// A list of two files, the first with two hunks, and a comment.
fn list() -> ReviewList {
    ReviewList {
        chat: Some(CHAT),
        chat_name: "Claude Code".to_owned(),
        scope: Some(ReviewScope::Chat),
        files: vec![
            ReviewFile {
                path: "/src/a.rs".to_owned(),
                shown: "a.rs".to_owned(),
                buffer: None,
                hunks: vec![
                    hunk(2, ("    old();\n", "    new();\n    more();\n"), 1, 0),
                    hunk(10, ("    gone();\n", ""), 2, 1),
                ],
            },
            ReviewFile {
                path: "/src/b.rs".to_owned(),
                shown: "b.rs".to_owned(),
                buffer: None,
                hunks: vec![hunk(5, ("", "    added();\n"), 3, 1)],
            },
        ],
        comments: vec![ReviewComment {
            id: 9,
            path: "/src/a.rs".to_owned(),
            shown: "a.rs".to_owned(),
            line: 2,
            text: "why?".to_owned(),
            in_change: true,
        }],
        note: None,
        chats: Vec::new(),
    }
}

fn review() -> Review {
    let mut review = Review::new(ReviewId(1), &Options::default());
    review.resize(Size::new(60, 30).unwrap());
    review.update(list());
    review
}

fn keys(review: &mut Review, keys: &str) -> Vec<Ask> {
    for stroke in Stroke::read(keys) {
        review.stroke(stroke);
    }
    review.take_asks()
}

/// The rows' text, trimmed.
fn rows(review: &mut Review) -> Vec<String> {
    review.settle();
    let (rows, _) = review.scene();
    (rows.iter())
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

#[test]
fn the_keys_line_is_cut_short_with_an_ellipsis_in_a_narrow_pane() {
    let mut review = review();
    review.resize(Size::new(30, 30).unwrap());
    let shown = rows(&mut review);
    assert_eq!(shown[1], "a keep  x reject  …", "whole keys only");
    // Narrower than one key, it's cut.
    review.resize(Size::new(5, 30).unwrap());
    assert_eq!(rows(&mut review)[1], "a ke…");
    // It's whole where it fits.
    review.resize(Size::new(120, 30).unwrap());
    assert!(rows(&mut review)[1].ends_with("q close"));
}

#[test]
fn hunks_show_the_lines_around_them_old_on_red_and_new_on_green() {
    let mut review = review();
    let shown = rows(&mut review);
    assert_eq!(shown[0], "Review of the chat · Claude Code");
    let at = shown.iter().position(|row| row == "a.rs  2 hunks").unwrap();
    assert_eq!(
        &shown[at + 1..at + 7],
        [
            "@@ line 2 · Claude Code, turn 1",
            "    1   fn main() {",
            "      -     old();",
            "    2 +     new();",
            "    3 +     more();",
            "    4   }",
        ]
    );
    let lit: Vec<(usize, HighlightKind)> = (review.highlights().into_iter())
        .filter(|highlight| highlight.kind != HighlightKind::CursorLine)
        .map(|highlight| (highlight.line, highlight.kind))
        .collect();
    assert_eq!(
        &lit[..3],
        [
            (at + 3, HighlightKind::Removed),
            (at + 4, HighlightKind::Added),
            (at + 5, HighlightKind::Added)
        ]
    );
    assert!(shown.contains(&"✎ a.rs:2 why?".to_owned()));
}

#[test]
fn brackets_go_between_hunks_and_files_and_round() {
    let mut review = review();
    // The cursor starts on the first hunk.
    assert_eq!(review.label(), "a.rs  hunk 1 of 3");
    keys(&mut review, "]h");
    assert_eq!(review.label(), "a.rs  hunk 2 of 3");
    keys(&mut review, "]h");
    assert_eq!(review.label(), "b.rs  hunk 3 of 3");
    keys(&mut review, "]h");
    assert_eq!(review.label(), "a.rs  hunk 1 of 3", "round to the first");
    keys(&mut review, "[h");
    assert_eq!(review.label(), "b.rs  hunk 3 of 3");
    keys(&mut review, "[f");
    assert_eq!(review.label(), "b.rs  1 hunk");
    keys(&mut review, "[f");
    assert_eq!(review.label(), "a.rs  2 hunks");
    keys(&mut review, "]h");
    assert_eq!(review.label(), "a.rs  hunk 1 of 3");
}

#[test]
fn keys_keep_reject_comment_send_and_go_to_the_line() {
    let mut review = review();
    let keep = |ids: Vec<u64>| vec![Ask::Act(ReviewAction::Keep(Hunks::Ids(ids)))];
    let reject = |ids: Vec<u64>| {
        vec![Ask::Act(ReviewAction::Reject {
            hunks: Hunks::Ids(ids),
            force: false,
        })]
    };
    assert_eq!(keys(&mut review, "a"), keep(vec![1]));
    assert_eq!(keys(&mut review, "x"), reject(vec![1]));
    assert_eq!(keys(&mut review, "A"), keep(vec![1, 2]));
    assert_eq!(keys(&mut review, "X"), reject(vec![1, 2]));
    // On the hunk's first line, Enter and gc are for its first line.
    assert_eq!(
        keys(&mut review, "<CR>"),
        [Ask::Open {
            path: "/src/a.rs".into(),
            line: 1
        }]
    );
    // On a line of its own, for that line.
    keys(&mut review, "jjjj");
    assert_eq!(
        keys(&mut review, "gc"),
        [Ask::Comment {
            chat: CHAT,
            path: "/src/a.rs".into(),
            line: 3
        }]
    );
    assert_eq!(keys(&mut review, "S"), [Ask::Act(ReviewAction::Send(CHAT))]);
    assert_eq!(keys(&mut review, "q"), [Ask::Close]);
    // On a comment, x takes it back.
    keys(&mut review, "G");
    assert_eq!(
        keys(&mut review, "x"),
        [Ask::Act(ReviewAction::Uncomment { chat: CHAT, id: 9 })]
    );
}

#[test]
fn a_subagents_hunks_say_whose_and_the_heading_says_undoing_leaves_them() {
    let mut review = review();
    let mut with = list();
    with.scope = Some(ReviewScope::Turn(Some(1)));
    let sub = TerminalId(7);
    with.files[1].hunks[0].by = Some(Made { chat: sub, turn: 0 });
    with.chats = vec![
        clackatoo_protocol::ReviewChat {
            chat: CHAT,
            name: "Claude Code".to_owned(),
            task: None,
            turn: None,
            closed: false,
        },
        clackatoo_protocol::ReviewChat {
            chat: sub,
            name: "explore (chat 7)".to_owned(),
            task: Some("find the parser".to_owned()),
            turn: Some(1),
            closed: true,
        },
    ];
    review.update(with);
    let shown = rows(&mut review);
    assert_eq!(
        &shown[3..5],
        [
            "↳ explore (chat 7): find the parser  closed",
            "Undoing turn 2 leaves subagents' changes: reject them here.",
        ]
    );
    assert!(shown.contains(&"@@ line 5 · explore (chat 7), turn 1".to_owned()));
    // On its hunk, the turn is the review's chat's that started it.
    keys(&mut review, "]h]h");
    assert_eq!(review.label(), "b.rs  hunk 3 of 3");
    assert_eq!(review.turn_here(), Some(1));
    keys(&mut review, "]h");
    assert_eq!(review.turn_here(), Some(0));
}

#[test]
fn a_change_of_the_users_own_is_neither_kept_nor_rejected() {
    let mut review = review();
    let mut mine = list();
    mine.scope = Some(ReviewScope::Base);
    mine.files[0].hunks[0].agents.clear();
    mine.files[0].hunks[0].by = None;
    review.update(mine);
    assert!(keys(&mut review, "a").is_empty());
    assert_eq!(review.label(), "this change is yours, not an agent's");
    assert!(rows(&mut review).contains(&"@@ line 2 · yours".to_owned()));
}

#[test]
fn the_cursor_stays_on_its_hunk_as_the_list_changes() {
    let mut review = review();
    keys(&mut review, "]h]h");
    assert_eq!(review.label(), "b.rs  hunk 3 of 3");
    // The first file's hunks kept and gone: b.rs's is still the cursor's.
    let mut fewer = list();
    fewer.files.remove(0);
    review.update(fewer);
    assert_eq!(review.label(), "b.rs  hunk 1 of 1");
    // All gone: the note says so.
    review.update(ReviewList {
        note: Some("Nothing to review.".to_owned()),
        ..ReviewList::default()
    });
    assert!(rows(&mut review).contains(&"Nothing to review.".to_owned()));
    assert!(keys(&mut review, "a").is_empty());
}

#[test]
fn brackets_take_counts_without_walking_them_and_show_while_half_typed() {
    let mut review = review();
    keys(&mut review, "2]h");
    assert_eq!(review.label(), "b.rs  hunk 3 of 3");
    // Round as many times as a count says comes back where it was.
    keys(&mut review, "99999999]h");
    assert_eq!(review.label(), "b.rs  hunk 3 of 3");
    keys(&mut review, "4[h");
    assert_eq!(review.label(), "a.rs  hunk 2 of 3");
    // What's half typed shows first in the bar, as Vim's showcmd does.
    keys(&mut review, "3]");
    assert!(review.pending());
    assert_eq!(review.label(), "3]  a.rs  hunk 2 of 3");
    keys(&mut review, "<Esc>");
    assert!(!review.pending());
}

#[test]
fn the_lists_moves_work_in_a_review() {
    let mut review = review();
    review.resize(Size::new(60, 10).unwrap());
    review.settle();
    keys(&mut review, "gg");
    assert_eq!(review.cursor, 0);
    keys(&mut review, "5G");
    assert_eq!(review.cursor, 4);
    keys(&mut review, "L");
    assert_eq!(review.cursor, 9);
    keys(&mut review, "H");
    assert_eq!(review.cursor, 0);
    keys(&mut review, "<C-e>");
    assert_eq!((review.cursor, review.top), (1, 1));
    keys(&mut review, "<C-d>");
    assert_eq!((review.cursor, review.top), (6, 6));
    keys(&mut review, "zt");
    assert_eq!(review.top, 6);
    // Keys a review doesn't take spend what was typed.
    keys(&mut review, "dw");
    assert!(!review.pending());
    assert_eq!(review.cursor, 6);
    assert!(keys(&mut review, "\"ax").is_empty(), "no registers");
}

#[test]
fn searches_find_lines_files_and_comments() {
    let mut review = review();
    keys(&mut review, "/more");
    assert_eq!(review.label(), "/more▏");
    keys(&mut review, "<CR>");
    assert!(
        matches!(&review.lines[review.cursor], Line::Text { text, .. } if text.contains("more();"))
    );
    keys(&mut review, "/b.rs<CR>");
    assert_eq!(review.label(), "b.rs  1 hunk");
    keys(&mut review, "/why<CR>");
    assert!(matches!(review.lines[review.cursor], Line::Comment { .. }));
    keys(&mut review, "/nowhere<CR>");
    assert_eq!(review.label(), "no line matches nowhere");
    // `?` goes back; `n` the way it went, `N` the other.
    keys(&mut review, "gg?\\.rs<CR>");
    assert_eq!(review.label(), "search wrapped to bottom");
    keys(&mut review, "<Esc>");
    assert_eq!(review.label(), "b.rs  1 hunk");
    keys(&mut review, "n");
    assert_eq!(review.label(), "a.rs  2 hunks");
    keys(&mut review, "N");
    assert_eq!(review.label(), "b.rs  1 hunk");
}

#[test]
fn its_keys_help_comes_from_how_it_reads_them() {
    let mut review = review();
    let rows = review.shortcut_context().rows;
    let has = |rows: &[(String, String)], key: &str, said: &str| {
        rows.iter().any(|(k, d)| k == key && d == said)
    };
    assert!(has(&rows, "]h", "Next hunk"), "{rows:?}");
    assert!(has(&rows, "gc", "Comment on this line"));
    assert!(has(&rows, "zt", "Cursor line at top"));
    assert!(has(&rows, "/", "Search forward"));
    assert!(!rows.iter().any(|(key, _)| key == "dd"), "no operators");
    keys(&mut review, "]");
    let after = review.shortcut_context();
    assert!(after.title.contains("after ]"), "{}", after.title);
    assert!(has(&after.rows, "f", "Next file"));
}

#[test]
fn a_key_that_does_nothing_still_clears_a_message_and_redraws() {
    let mut review = review();
    keys(&mut review, "/nowhere<CR>");
    assert_eq!(review.label(), "no line matches nowhere");
    assert!(review.stroke(Stroke::Escape), "drawn again");
    assert_ne!(review.label(), "no line matches nowhere");
}

#[test]
fn a_search_lights_what_it_matches_where_its_drawn() {
    let mut review = review();
    keys(&mut review, "/r");
    crate::vim::search::preview(&mut Searching(&mut review));
    assert_eq!(review.label(), "/r▏");
    keys(&mut review, "<CR>");
    review.settle();
    let shown = rows(&mut review);
    let lit: Vec<(String, HighlightKind)> = (review.highlights().into_iter())
        .filter(|highlight| {
            matches!(
                highlight.kind,
                HighlightKind::Match | HighlightKind::OtherMatch
            )
        })
        .map(|highlight| {
            let row: Vec<char> = shown[highlight.line].chars().collect();
            (row[highlight.cols].iter().collect(), highlight.kind)
        })
        .collect();
    // In files' names, changed lines and comments, the one gone to
    // brighter.
    assert!(lit.contains(&("r".into(), HighlightKind::Match)), "{lit:?}");
    assert_eq!(
        lit.iter()
            .filter(|(_, kind)| *kind == HighlightKind::Match)
            .count(),
        1
    );
    assert_eq!(lit.len(), 3, "{lit:?}");
    assert!(lit.iter().all(|(text, _)| text == "r"), "{lit:?}");
    // `:nohlsearch`'s Esc puts them out.
    keys(&mut review, "<Esc>");
    review.settle();
    assert!(
        review
            .highlights()
            .iter()
            .all(|highlight| highlight.kind != HighlightKind::OtherMatch)
    );
}

#[test]
fn what_a_search_keeps_follows_the_lines_as_the_list_changes() {
    let mut review = review();
    keys(&mut review, "/added<CR>");
    let found = review.search.found().expect("found");
    assert!(matches!(&review.lines[found], Line::Text { text, .. } if text.contains("added")));
    // A file comes before them: the match found is still on its line.
    let mut more = list();
    more.files.insert(
        0,
        ReviewFile {
            path: "/src/0.rs".to_owned(),
            shown: "0.rs".to_owned(),
            buffer: None,
            hunks: vec![hunk(1, ("", "    first();\n"), 4, 0)],
        },
    );
    review.update(more.clone());
    let moved = review.search.found().expect("still found");
    assert!(moved > found);
    assert!(matches!(&review.lines[moved], Line::Text { text, .. } if text.contains("added")));
    // A search typed from b.rs's line, shown elsewhere: given up after the
    // list changes, the cursor's back on b.rs's line.
    keys(&mut review, "<Esc>");
    let b = (review.lines.iter())
        .position(
            |line| matches!(line, Line::File { file } if review.list.files[*file].shown == "b.rs"),
        )
        .expect("b.rs");
    review.cursor = b;
    keys(&mut review, "/first");
    crate::vim::search::preview(&mut Searching(&mut review));
    assert_ne!(review.cursor, b);
    review.update(list());
    keys(&mut review, "<Esc>");
    assert_eq!(review.label(), "b.rs  1 hunk");
    // A match whose hunk's gone is lit no more.
    review.update(more);
    keys(&mut review, "/first<CR>");
    assert!(review.search.found().is_some());
    review.update(list());
    assert_eq!(review.search.found(), None);
}

#[test]
fn what_is_lit_is_looked_for_once_and_only_as_far_as_is_drawn() {
    let mut long = list();
    // A changed line far wider than the pane, matching all along it.
    long.files[1].hunks[0].text = format!("{}\n", "ab".repeat(100_000));
    let mut review = Review::new(ReviewId(1), &Options::default());
    review.resize(Size::new(60, 30).unwrap());
    review.update(long);
    keys(&mut review, "/b<CR>");
    let scans = || crate::hlsearch::SCANS.with(std::cell::Cell::get);
    let before = scans();
    let shown = rows(&mut review);
    assert_eq!(scans(), before + 1);
    // Drawn again as it was, it's not looked for again.
    rows(&mut review);
    assert_eq!(scans(), before + 1);
    // Only those drawn, in the columns they're drawn in.
    let on_long: Vec<std::ops::Range<usize>> = (review.highlights().into_iter())
        .filter(|highlight| {
            matches!(
                highlight.kind,
                HighlightKind::Match | HighlightKind::OtherMatch
            ) && shown[highlight.line].contains("abab")
        })
        .map(|highlight| highlight.cols)
        .collect();
    assert!(!on_long.is_empty());
    assert!(on_long.len() <= 30, "{}", on_long.len());
    assert!(on_long.iter().all(|cols| cols.end <= 60));
    // A tab is as wide as drawn.
    let mut tabbed = list();
    tabbed.files[1].hunks[0].text = "\tadded();\n".to_owned();
    review.update(tabbed);
    keys(&mut review, "/added<CR>");
    let shown = rows(&mut review);
    let lit: Vec<String> = (review.highlights().into_iter())
        .filter(|highlight| highlight.kind == HighlightKind::Match)
        .map(|highlight| {
            let row: Vec<char> = shown[highlight.line].chars().collect();
            row[highlight.cols].iter().collect()
        })
        .collect();
    assert_eq!(lit, ["added"]);
}
