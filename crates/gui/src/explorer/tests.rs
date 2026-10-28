use std::time::Instant;

use super::*;

fn explorer(rows: u16) -> Explorer {
    let mut explorer = Explorer::new(FilesId(1), &Options::default());
    explorer.resize(Size::new(40, rows).unwrap());
    explorer
}

fn file(name: &str) -> Entry {
    Entry {
        name: name.into(),
        kind: EntryKind::File { executable: false },
        ignored: false,
        stamp: None,
    }
}

fn dir(name: &str) -> Entry {
    Entry {
        name: name.into(),
        kind: EntryKind::Dir,
        ignored: false,
        stamp: None,
    }
}

fn ignored(mut entry: Entry) -> Entry {
    entry.ignored = true;
    entry
}

/// Roots `explorer` at `/p`, listing what's in it as `entries`.
fn rooted(explorer: &mut Explorer, entries: Vec<Entry>) {
    explorer.update(FilesUpdate::Root {
        path: "/p".into(),
        name: "~/p".into(),
    });
    assert_eq!(explorer.take_outbox(), [FilesAction::Expand("/p".into())]);
    listed(explorer, "/p", entries);
}

fn listed(explorer: &mut Explorer, dir: &str, entries: Vec<Entry>) {
    explorer.update(FilesUpdate::Listed {
        dir: dir.into(),
        entries,
        cut_short: false,
    });
}

fn keys(explorer: &mut Explorer, keys: &str) {
    for stroke in Stroke::read(keys) {
        explorer.stroke(stroke);
    }
}

/// The rows drawn, as text, without the blank ones at the end.
fn shown(explorer: &mut Explorer) -> Vec<String> {
    explorer.settle();
    let (rows, _) = explorer.scene();
    let mut text: Vec<String> = rows.iter().map(|row| row.text()).collect();
    while text.last().is_some_and(String::is_empty) {
        text.pop();
    }
    text
}

/// The path at the cursor.
fn at(explorer: &mut Explorer) -> String {
    explorer.settle();
    explorer.line().expect("a line at the cursor").path()
}

fn project() -> Vec<Entry> {
    vec![
        ignored(dir(".git")),
        dir("src"),
        ignored(dir("target")),
        file(".gitignore"),
        file("Cargo.toml"),
    ]
}

#[test]
fn directories_are_listed_as_theyre_expanded_and_forgotten_as_theyre_collapsed() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, project());
    assert_eq!(
        shown(&mut explorer),
        ["~/p", "    ..", "  ▸ src", "  ▸ target", "    Cargo.toml"],
        "hidden files hidden"
    );
    keys(&mut explorer, "l");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Expand("/p/src".into())]
    );
    assert_eq!(
        shown(&mut explorer)[2],
        "  ▾ src",
        "expanded before it's listed"
    );
    listed(&mut explorer, "/p/src", vec![dir("bin"), file("main.rs")]);
    assert_eq!(
        shown(&mut explorer),
        [
            "~/p",
            "    ..",
            "  ▾ src",
            "  │ ▸ bin",
            "  │   main.rs",
            "  ▸ target",
            "    Cargo.toml"
        ]
    );
    // l goes into an expanded directory, and expands one; h collapses
    // it, then goes back out.
    keys(&mut explorer, "ll");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Expand("/p/src/bin".into())]
    );
    listed(&mut explorer, "/p/src/bin", vec![file("x.rs")]);
    keys(&mut explorer, "h");
    assert_eq!(at(&mut explorer), "/p/src/bin");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Collapse("/p/src/bin".into())]
    );
    keys(&mut explorer, "h");
    assert_eq!(at(&mut explorer), "/p/src");
    keys(&mut explorer, "h");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Collapse("/p/src".into())]
    );
    assert_eq!(shown(&mut explorer).len(), 5);
    // Expanded again, it's listed again; what was expanded in it isn't.
    keys(&mut explorer, "zo");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Expand("/p/src".into())]
    );

    // A listing that comes after a collapse is ignored.
    keys(&mut explorer, "zc");
    explorer.take_outbox();
    listed(&mut explorer, "/p/src", vec![file("late.rs")]);
    assert_eq!(shown(&mut explorer).len(), 5);
}

#[test]
fn a_directory_inside_one_collapsed_stays_expanded_for_when_its_shown_again() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, project());
    keys(&mut explorer, "l");
    listed(&mut explorer, "/p/src", vec![dir("bin")]);
    keys(&mut explorer, "jl");
    listed(&mut explorer, "/p/src/bin", vec![file("x.rs")]);
    explorer.take_outbox();
    keys(&mut explorer, "kh");
    assert_eq!(
        explorer.take_outbox(),
        [
            FilesAction::Collapse("/p/src".into()),
            FilesAction::Collapse("/p/src/bin".into())
        ]
    );
    keys(&mut explorer, "l");
    assert_eq!(
        explorer.take_outbox(),
        [
            FilesAction::Expand("/p/src".into()),
            FilesAction::Expand("/p/src/bin".into())
        ]
    );
    // zM collapses everything, the cursor going to the top.
    listed(&mut explorer, "/p/src", vec![dir("bin")]);
    listed(&mut explorer, "/p/src/bin", vec![file("x.rs")]);
    keys(&mut explorer, "jjzM");
    assert_eq!(at(&mut explorer), "/p/src");
    keys(&mut explorer, "l");
    assert_eq!(
        explorer.take_outbox().len(),
        3,
        "the collapses, then src alone"
    );
}

#[test]
fn keys_move_with_counts_and_pages_and_the_view_follows_the_cursor() {
    let mut explorer = explorer(11);
    let entries = (0..100).map(|n| file(&format!("f{n:03}"))).collect();
    rooted(&mut explorer, entries);
    keys(&mut explorer, "5j");
    assert_eq!(at(&mut explorer), "/p/f005");
    keys(&mut explorer, "G");
    assert_eq!(at(&mut explorer), "/p/f099");
    assert_eq!(shown(&mut explorer).last().unwrap(), "    f099");
    // `..` is a line as any other: the first.
    keys(&mut explorer, "gg");
    assert_eq!(explorer.label(), "~/p  ..  g? keys");
    keys(&mut explorer, "j");
    assert_eq!(at(&mut explorer), "/p/f000");
    keys(&mut explorer, "12G");
    assert_eq!(at(&mut explorer), "/p/f010");
    keys(&mut explorer, "3gg");
    assert_eq!(at(&mut explorer), "/p/f001");
    keys(&mut explorer, "2k");
    assert!(explorer.on_up());
    keys(&mut explorer, "<C-d>");
    assert_eq!(at(&mut explorer), "/p/f004", "half of the ten lines shown");
    keys(&mut explorer, "<C-u>");
    assert!(explorer.on_up());
    keys(&mut explorer, "<C-f>");
    assert_eq!(at(&mut explorer), "/p/f007");
    // Ctrl+B is the window's prefix, never the pane's: Page Up pages up,
    // the cursor to the view's bottom, as Neovim's.
    keys(&mut explorer, "<PageUp>");
    assert_eq!(at(&mut explorer), "/p/f008");
    keys(&mut explorer, "ggk");
    assert!(explorer.on_up(), "k stops at ..");
    keys(&mut explorer, "<Home>j<Up>");
    assert!(explorer.on_up());
}

#[test]
fn slash_searches_the_names_shown_and_n_goes_on() {
    let mut explorer = explorer(10);
    rooted(
        &mut explorer,
        vec![
            file("alpha.rs"),
            file("beta.md"),
            file("gamma.rs"),
            file("Delta.RS"),
        ],
    );
    keys(&mut explorer, "/\\.rs$<CR>");
    assert_eq!(at(&mut explorer), "/p/gamma.rs");
    keys(&mut explorer, "n");
    assert_eq!(at(&mut explorer), "/p/Delta.RS", "case ignored");
    keys(&mut explorer, "n");
    assert_eq!(at(&mut explorer), "/p/alpha.rs", "round from the end");
    keys(&mut explorer, "N");
    assert_eq!(at(&mut explorer), "/p/Delta.RS");
    let lit = |kind| -> Vec<usize> {
        (explorer.highlights().iter())
            .filter(|highlight| highlight.kind == kind)
            .map(|highlight| highlight.line)
            .collect()
    };
    assert_eq!(lit(HighlightKind::Match), [5], "the one gone to");
    assert_eq!(
        lit(HighlightKind::OtherMatch),
        [2, 4],
        "below the root's row and ..'s"
    );
    keys(&mut explorer, "/zzz<CR>");
    assert_eq!(explorer.label(), "no name shown matches zzz");
    assert_eq!(at(&mut explorer), "/p/Delta.RS");
}

#[test]
fn git_marks_show_beside_files_and_roll_up_into_their_directories() {
    let mut explorer = explorer(10);
    rooted(
        &mut explorer,
        vec![
            dir("new"),
            dir("src"),
            file("a.txt"),
            file("b.txt"),
            ignored(file("x.log")),
        ],
    );
    explorer.update(FilesUpdate::Git(vec![
        ("/p/a.txt".into(), GitMark::Modified),
        ("/p/new".into(), GitMark::Untracked),
        ("/p/src/deep/c.rs".into(), GitMark::Added),
        ("/p/src/d.rs".into(), GitMark::Untracked),
    ]));
    assert_eq!(
        shown(&mut explorer),
        [
            "~/p",
            "    ..",
            "? ▸ new",
            "M ▸ src",
            "M   a.txt",
            "    b.txt",
            "    x.log"
        ]
    );
    let (rows, _) = explorer.scene();
    let style = |row: &Row, col: usize| *row.style(row.cells()[col]);
    assert_eq!(
        style(rows[3], 0).fg,
        colors::git_mark(GitMark::Modified).1.fg
    );
    assert!(
        style(rows[6], 4).attributes.contains(Attributes::DIM),
        "ignored"
    );
    assert!(!style(rows[5], 4).attributes.contains(Attributes::DIM));
    // In a directory git says is untracked whole.
    keys(&mut explorer, "l");
    listed(&mut explorer, "/p/new", vec![file("n.rs")]);
    assert_eq!(shown(&mut explorer)[3], "? │   n.rs");
}

#[test]
// Every operation, in turn, on one tree.
#[allow(clippy::too_many_lines)]
fn files_are_opened_and_operated_on_as_asked() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, vec![dir("src"), file("a.txt")]);
    keys(&mut explorer, "j<CR><C-v><C-s><C-t>");
    let open = |how| Ask::Open {
        path: "/p/a.txt".into(),
        how,
    };
    assert_eq!(
        explorer.take_asks(),
        [
            open(Opening::Here),
            open(Opening::Split(Side::Right)),
            open(Opening::Split(Side::Below)),
            open(Opening::Tab)
        ]
    );
    // a makes a file where the cursor is: in the directory on, or the one
    // its line is in. A name ending / makes a directory.
    keys(&mut explorer, "anew.txt<CR>ka");
    assert!(explorer.prompting());
    assert_eq!(
        explorer.label(),
        "new file in ~/p/src/ (end with / for a directory): ▏"
    );
    keys(&mut explorer, "lib/<CR>");
    assert_eq!(
        explorer.take_asks(),
        [
            Ask::Op(FileOp::Create {
                path: "/p/new.txt".into(),
                dir: false
            }),
            Ask::Op(FileOp::Create {
                path: "/p/src/lib".into(),
                dir: true
            })
        ]
    );
    // r renames, from what it was called.
    keys(&mut explorer, "jr<BS><BS><BS>md<CR>");
    let moved = FileOp::Move {
        from: "/p/a.txt".into(),
        to: "/p/a.md".into(),
        overwrite: None,
        expect: None,
    };
    assert_eq!(explorer.take_asks(), [Ask::Op(moved.clone())]);
    // Something there is written over only if the answer is yes.
    let stamp = clackatoo_protocol::FileStamp {
        device: 1,
        inode: 2,
        len: 3000,
        seconds: 4,
        nanoseconds: 5,
    };
    let there = FileOpError::Exists {
        path: "/p/a.md".into(),
        what: "a file of 3 KB".into(),
        stamp,
    };
    explorer.answered(0, moved, Err(there));
    assert_eq!(
        explorer.label(),
        "~/p/a.md is already there, a file of 3 KB: write over it, losing it? (y/n)"
    );
    keys(&mut explorer, "y");
    assert_eq!(
        explorer.take_asks(),
        [Ask::Op(FileOp::Move {
            from: "/p/a.txt".into(),
            to: "/p/a.md".into(),
            overwrite: Some(stamp),
            expect: None,
        })]
    );
    // d asks first, and deletes for good only if asked again.
    keys(&mut explorer, "dn");
    assert_eq!(explorer.take_asks(), []);
    assert_eq!(explorer.label(), "nothing was done");
    keys(&mut explorer, "dy");
    let trashed = FileOp::Delete {
        path: "/p/a.txt".into(),
        trash: true,
        expect: None,
    };
    assert_eq!(explorer.take_asks(), [Ask::Op(trashed.clone())]);
    explorer.answered(
        1,
        trashed,
        Err(FileOpError::NoTrash("no trash there".into())),
    );
    assert_eq!(
        explorer.label(),
        "no trash there: delete ~/p/a.txt for good? (y/n)"
    );
    keys(&mut explorer, "y");
    assert_eq!(
        explorer.take_asks(),
        [Ask::Op(FileOp::Delete {
            path: "/p/a.txt".into(),
            trash: false,
            expect: None
        })]
    );
    // x and y keep what's at the cursor; p puts it where the cursor is.
    keys(&mut explorer, "xkp<Esc>q");
    assert_eq!(
        explorer.take_asks(),
        [
            Ask::Keep {
                path: "/p/a.txt".into(),
                cut: true
            },
            Ask::Paste {
                dir: "/p/src".into()
            },
            Ask::Close
        ]
    );
}

#[test]
fn dot_shows_hidden_files_and_minus_goes_up_a_directory() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, project());
    keys(&mut explorer, ".");
    assert_eq!(
        shown(&mut explorer)[2],
        "  ▸ .git",
        "dimmed, as git ignores it"
    );
    keys(&mut explorer, "g.");
    assert_eq!(shown(&mut explorer)[2], "  ▸ src");

    keys(&mut explorer, "-");
    assert_eq!(explorer.take_outbox(), [FilesAction::Root("/".into())]);
    explorer.update(FilesUpdate::Root {
        path: "/".into(),
        name: "/".into(),
    });
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Expand("/".into())],
        "the old root is listed as it was"
    );
    listed(&mut explorer, "/", vec![dir("etc"), dir("p")]);
    listed(&mut explorer, "/p", project());
    assert_eq!(at(&mut explorer), "/p", "on the old root");
    assert_eq!(
        &shown(&mut explorer)[..3],
        ["/", "  ▸ etc", "  ▾ p"],
        "no .. above /"
    );
}

#[test]
fn a_file_revealed_has_what_its_in_expanded_and_the_cursor_put_on_it() {
    let mut explorer = explorer(10);
    explorer.reveal("/p/src/bin/x.rs".into());
    explorer.update(FilesUpdate::Root {
        path: "/p".into(),
        name: "~/p".into(),
    });
    assert_eq!(
        explorer.take_outbox(),
        [
            FilesAction::Expand("/p".into()),
            FilesAction::Expand("/p/src".into()),
            FilesAction::Expand("/p/src/bin".into())
        ],
        "all at once"
    );
    listed(&mut explorer, "/p", project());
    listed(
        &mut explorer,
        "/p/src/bin",
        vec![file("w.rs"), file("x.rs")],
    );
    listed(&mut explorer, "/p/src", vec![dir("bin"), file("main.rs")]);
    assert_eq!(at(&mut explorer), "/p/src/bin/x.rs");
    // Kept, it's shown again as it was.
    let view = explorer.view();
    let mut again = Explorer::new(FilesId(2), &Options::default());
    again.resize(Size::new(40, 10).unwrap());
    rooted(&mut again, project());
    again.restore(view);
    let mut asked = again.take_outbox();
    asked.sort_by_key(|action| format!("{action:?}"));
    assert_eq!(asked.len(), 2);
    listed(&mut again, "/p/src", vec![dir("bin"), file("main.rs")]);
    listed(&mut again, "/p/src/bin", vec![file("w.rs"), file("x.rs")]);
    assert_eq!(at(&mut again), "/p/src/bin/x.rs");
}

#[test]
fn a_directory_gone_is_collapsed_and_the_root_gone_says_so() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, project());
    keys(&mut explorer, "l");
    listed(&mut explorer, "/p/src", vec![file("main.rs")]);
    explorer.take_outbox();
    explorer.update(FilesUpdate::Unlisted {
        dir: "/p/src".into(),
        reason: "gone".into(),
    });
    assert_eq!(shown(&mut explorer)[2], "  ▸ src");
    keys(&mut explorer, "l");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Expand("/p/src".into())]
    );
    explorer.update(FilesUpdate::Unlisted {
        dir: "/p".into(),
        reason: "No such file or directory".into(),
    });
    assert_eq!(explorer.label(), "~/p: No such file or directory");
}

#[test]
fn git_that_cant_be_run_says_why() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, project());
    let why = "git 2.38 or newer is needed for git status, not 2.37.1";
    explorer.update(FilesUpdate::NoGit(why.into()));
    assert_eq!(explorer.label(), why);
}

/// Ten thousand files in a directory: the time to take them in, flatten
/// them and draw what's in view, and then to move the cursor and draw
/// again. Run with `--release --nocapture` for numbers worth reading.
#[test]
fn ten_thousand_files_cost_a_frame_what_ten_do() {
    let mut explorer = explorer(50);
    rooted(&mut explorer, vec![dir("big")]);
    let entries: Vec<Entry> = (0..10_000)
        .map(|n| file(&format!("file-{n:05}.rs")))
        .collect();
    keys(&mut explorer, "l");
    let started = Instant::now();
    listed(&mut explorer, "/p/big", entries);
    explorer.settle();
    let opened = started.elapsed();
    let started = Instant::now();
    for _ in 0..100 {
        keys(&mut explorer, "j");
        explorer.settle();
    }
    let moved = started.elapsed() / 100;
    let started = Instant::now();
    keys(&mut explorer, "G");
    explorer.settle();
    let to_end = started.elapsed();
    eprintln!("10,000 files: shown in {opened:?}, j and a frame in {moved:?}, G in {to_end:?}");
    assert_eq!(at(&mut explorer), "/p/big/file-09999.rs");
    assert_eq!(explorer.rows.len(), 50, "only what's in view is drawn");
}

#[test]
fn a_click_puts_the_cursor_on_a_line_a_double_click_opens_it_and_the_wheel_scrolls() {
    let mut explorer = explorer(6);
    let entries = (0..20).map(|n| file(&format!("f{n:02}"))).collect();
    rooted(&mut explorer, entries);
    explorer.settle();
    assert!(explorer.click(4, false));
    assert_eq!(at(&mut explorer), "/p/f02");
    assert!(!explorer.click(0, false), "the root's line");
    explorer.click(3, true);
    let open = Ask::Open {
        path: "/p/f01".into(),
        how: Opening::Here,
    };
    assert_eq!(explorer.take_asks(), [open]);
    assert!(explorer.scroll(10));
    assert_eq!(at(&mut explorer), "/p/f09", "brought into view");
    assert_eq!(shown(&mut explorer)[1], "    f09");
    explorer.scroll(-100);
    assert_eq!(shown(&mut explorer)[1], "    ..");
    assert_eq!(at(&mut explorer), "/p/f03");
}

/// Opening an explorer on this repository: the time from the daemon's
/// root and listing coming to the rows being ready to draw. Run with
/// `--release --nocapture` for numbers worth reading.
#[test]
fn opening_on_this_repository_takes_a_moment() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap();
    let mut entries: Vec<Entry> = (std::fs::read_dir(&root).unwrap().flatten())
        .map(|found| {
            let is_dir = found.file_type().is_ok_and(|kind| kind.is_dir());
            let name = found.file_name().to_string_lossy().into_owned();
            if is_dir { dir(&name) } else { file(&name) }
        })
        .collect();
    entries.sort_by_key(|entry| (!entry.kind.expands(), entry.name.to_lowercase()));
    let path = root.display().to_string();
    let started = Instant::now();
    let mut explorer = explorer(50);
    explorer.update(FilesUpdate::Root {
        path: path.clone(),
        name: "~/clackatoo".into(),
    });
    explorer.update(FilesUpdate::Listed {
        dir: path,
        entries,
        cut_short: false,
    });
    explorer.settle();
    let opened = started.elapsed();
    eprintln!("this repository's root: shown in {opened:?}");
    assert!(shown(&mut explorer).len() > 3);
}

#[test]
fn a_file_to_reveal_that_isnt_shown_is_forgotten_once_what_its_in_is() {
    let mut explorer = explorer(10);
    explorer.reveal("/p/.hidden".into());
    rooted(
        &mut explorer,
        vec![dir("src"), file(".hidden"), file("a.txt")],
    );
    keys(&mut explorer, "j");
    assert_eq!(at(&mut explorer), "/p/a.txt");
    // Shown later, it isn't jumped to.
    keys(&mut explorer, ".");
    assert_eq!(at(&mut explorer), "/p/a.txt");
}

#[test]
fn deleting_a_folder_for_good_says_it_deletes_everything_in_it() {
    let mut explorer = explorer(10);
    let link = Entry {
        name: "linked".into(),
        kind: EntryKind::Link { dir: true },
        ignored: false,
        stamp: None,
    };
    rooted(&mut explorer, vec![dir("src"), link]);
    keys(&mut explorer, "d");
    assert_eq!(
        explorer.label(),
        "delete ~/p/src and everything in it, to the trash? (y/n)"
    );
    keys(&mut explorer, "y");
    let [Ask::Op(trashed)] = &explorer.take_asks()[..] else {
        panic!("no delete asked");
    };
    explorer.answered(
        0,
        trashed.clone(),
        Err(FileOpError::NoTrash("no trash there".into())),
    );
    assert_eq!(
        explorer.label(),
        "no trash there: delete ~/p/src and everything in it for good? (y/n)"
    );
    keys(&mut explorer, "n");
    // A link to a folder is deleted alone.
    keys(&mut explorer, "jd");
    assert_eq!(explorer.label(), "delete ~/p/linked, to the trash? (y/n)");
}

#[test]
fn g_question_lists_the_keys_at_the_foot_and_again_hides_them() {
    let mut explorer = explorer(14);
    explorer.resize(Size::new(30, 14).unwrap());
    let entries = (0..20).map(|n| file(&format!("f{n:02}"))).collect();
    rooted(&mut explorer, entries);
    assert!(explorer.label().ends_with("g? keys"), "the bar says how");
    keys(&mut explorer, "g?");
    let rows = shown(&mut explorer);
    assert_eq!(rows.len(), 14, "the pane's height, no more");
    assert_eq!(
        rows[4..],
        [
            "",
            "a new · r rename · d delete",
            "- up · R refresh · l open",
            "h collapse · x cut · y copy",
            "p paste · e edit as text",
            "/ search · . hidden files",
            "zR expand all",
            "zM collapse all · Ctrl+V split",
            "Ctrl+S below · Ctrl+T tab",
            "q close · g? hide keys",
        ],
        "a key and what it does kept together, as many to a row as fit"
    );
    assert!(!explorer.label().contains("g?"));
    // The tree is the three rows above them: the cursor stays in view
    // there.
    keys(&mut explorer, "G");
    let rows = shown(&mut explorer);
    assert_eq!(rows[3], "    f19");
    // A click on the keys does nothing.
    keys(&mut explorer, "gg");
    shown(&mut explorer);
    assert!(!explorer.click(4, false));
    assert!(!explorer.click(9, false));
    assert!(explorer.click(2, false));
    // Too narrow for a key and what it does, it's cut short.
    explorer.resize(Size::new(12, 30).unwrap());
    assert!(shown(&mut explorer).contains(&"e edit as t…".to_owned()));
    // Too many for the pane, as many as fit, then how to hide them.
    explorer.resize(Size::new(12, 14).unwrap());
    let rows = shown(&mut explorer);
    assert_eq!(rows.len(), 14);
    assert_eq!(rows[12..], ["p paste", "… g? hide k…"]);
    // In a short pane they leave the heading and a line of the tree...
    explorer.resize(Size::new(30, 5).unwrap());
    let rows = shown(&mut explorer);
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows[2..],
        ["", "a new · r rename · d delete", "… g? hide keys"]
    );
    // ...and in one shorter still, there are none.
    explorer.resize(Size::new(30, 4).unwrap());
    assert!(!shown(&mut explorer).iter().any(|row| row.contains("g?")));
    explorer.resize(Size::new(30, 5).unwrap());
    keys(&mut explorer, "g?");
    assert_eq!(shown(&mut explorer).len(), 5, "the tree to the foot");
    assert!(
        !shown(&mut explorer)
            .iter()
            .any(|row| row.contains("rename"))
    );
}

#[test]
fn the_heading_is_cut_from_its_start_and_a_lesson_names_its_own() {
    let mut explorer = explorer(6);
    explorer.resize(Size::new(12, 6).unwrap());
    explorer.update(FilesUpdate::Root {
        path: "/home/me/.cache/clackatoo/tutor/116/explorer".into(),
        name: "~/.cache/clackatoo/tutor/116/explorer".into(),
    });
    assert_eq!(shown(&mut explorer)[0], "…16/explorer");
    // Written with a slash after it, it's still the root's folder.
    explorer.fence("/home/me/.cache/clackatoo/tutor/116/explorer/".into());
    explorer.name_fence("Practice: The file explorer".into());
    assert_eq!(shown(&mut explorer)[0], "Practice: T…", "at the fence");
    explorer.resize(Size::new(40, 6).unwrap());
    assert_eq!(shown(&mut explorer)[0], "Practice: The file explorer");
    // Deeper, it's the path again.
    explorer.update(FilesUpdate::Root {
        path: "/home/me/.cache/clackatoo/tutor/116/explorer/drafts".into(),
        name: "drafts".into(),
    });
    assert_eq!(shown(&mut explorer)[0], "drafts");
}

#[test]
fn an_explorer_kept_in_a_directory_goes_no_higher() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, project());
    explorer.fence("/p".into());
    keys(&mut explorer, "-");
    assert_eq!(explorer.take_outbox(), []);
    assert_eq!(explorer.label(), FENCED);
    explorer.root_at("/etc".into());
    assert_eq!(explorer.take_outbox(), []);
    explorer.root_at("src".into());
    assert_eq!(explorer.take_outbox(), [FilesAction::Root("/p/src".into())]);
    assert!(explorer.fenced_out("/home"));
    assert!(!explorer.fenced_out("/p/src/main.rs"));
}

#[test]
fn what_the_daemon_has_listed_is_looked_up_by_its_path_from_the_root() {
    let mut explorer = explorer(10);
    assert!(
        !explorer.lists("Cargo.toml"),
        "nothing's listed before a root"
    );
    rooted(&mut explorer, project());
    assert!(explorer.lists("Cargo.toml"));
    assert!(explorer.lists("src"));
    assert!(!explorer.lists("src/main.rs"), "src isn't listed yet");
    keys(&mut explorer, "l");
    assert_eq!(at(&mut explorer), "/p/src");
    listed(&mut explorer, "/p/src", vec![file("main.rs")]);
    assert!(explorer.lists("src/main.rs"));
    assert!(!explorer.lists("main.rs"));
}

/// Roots `explorer` at `/home/me/p`, listing what's in it as `entries`.
fn rooted_deeper(explorer: &mut Explorer, entries: Vec<Entry>) {
    explorer.update(FilesUpdate::Root {
        path: "/home/me/p".into(),
        name: "~/p".into(),
    });
    explorer.take_outbox();
    listed(explorer, "/home/me/p", entries);
}

/// Goes up from `/home/me/p`, as `typed` does, and has the daemon
/// answer: `/home/me` is the root, listing `p` and `q`, with `p` as it
/// was.
fn gone_up(explorer: &mut Explorer, typed: &str) {
    keys(explorer, typed);
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Root("/home/me".into())]
    );
    explorer.update(FilesUpdate::Root {
        path: "/home/me".into(),
        name: "~".into(),
    });
    listed(explorer, "/home/me", vec![dir("p"), dir("q")]);
    listed(explorer, "/home/me/p", project());
    explorer.take_outbox();
}

#[test]
fn dot_dot_tops_the_tree_and_enter_on_it_goes_up_to_the_folder_come_from() {
    let mut explorer = explorer(10);
    rooted_deeper(&mut explorer, project());
    // It opens on the first entry, not on `..`.
    assert_eq!(at(&mut explorer), "/home/me/p/src");
    assert_eq!(
        shown(&mut explorer),
        ["~/p", "    ..", "  ▸ src", "  ▸ target", "    Cargo.toml"]
    );
    // Moved onto as any line is: k from the first, and gg.
    keys(&mut explorer, "k");
    assert!(explorer.on_up());
    assert_eq!(explorer.label(), "~/p  ..  g? keys");
    keys(&mut explorer, "jgg");
    assert!(explorer.on_up());
    // A listing come again doesn't move it off.
    listed(&mut explorer, "/home/me/p", project());
    assert!(explorer.on_up());
    // Enter goes up, as `-` does, landing on the folder come from, open.
    gone_up(&mut explorer, "<CR>");
    assert_eq!(at(&mut explorer), "/home/me/p");
    assert_eq!(
        &shown(&mut explorer)[..4],
        ["~", "    ..", "  ▾ p", "  │ ▸ src"]
    );
    // And `l` does too.
    keys(&mut explorer, "ggl");
    assert_eq!(explorer.take_outbox(), [FilesAction::Root("/home".into())]);
}

#[test]
fn a_double_click_on_dot_dot_goes_up_and_a_single_one_only_moves_there() {
    let mut explorer = explorer(10);
    rooted_deeper(&mut explorer, project());
    explorer.settle();
    assert!(explorer.click(1, false));
    assert!(explorer.on_up());
    assert_eq!(explorer.take_outbox(), [], "a single click goes nowhere");
    explorer.click(2, false);
    assert_eq!(at(&mut explorer), "/home/me/p/src");
    explorer.click(1, true);
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Root("/home/me".into())]
    );
    explorer.update(FilesUpdate::Root {
        path: "/home/me".into(),
        name: "~".into(),
    });
    listed(&mut explorer, "/home/me", vec![dir("p"), dir("q")]);
    assert_eq!(at(&mut explorer), "/home/me/p");
}

#[test]
fn there_is_no_dot_dot_at_slash_nor_above_a_fence() {
    let mut explorer = explorer(10);
    explorer.update(FilesUpdate::Root {
        path: "/".into(),
        name: "/".into(),
    });
    listed(&mut explorer, "/", vec![dir("etc"), dir("home")]);
    assert_eq!(shown(&mut explorer), ["/", "  ▸ etc", "  ▸ home"]);
    keys(&mut explorer, "jgg");
    assert_eq!(at(&mut explorer), "/etc", "gg is the first entry");
    // Gone up to `/` from `..`, the cursor goes to the folder come from.
    let mut explorer = super::tests::explorer(10);
    rooted(&mut explorer, project());
    keys(&mut explorer, "gg<CR>");
    assert_eq!(explorer.take_outbox(), [FilesAction::Root("/".into())]);
    explorer.update(FilesUpdate::Root {
        path: "/".into(),
        name: "/".into(),
    });
    listed(&mut explorer, "/", vec![dir("etc"), dir("p")]);
    assert_eq!(at(&mut explorer), "/p");
    // Another window rooting it at `/` while the cursor's on `..`: it
    // goes to a line, as it does when the line it was on goes.
    let mut explorer = super::tests::explorer(10);
    rooted(&mut explorer, project());
    keys(&mut explorer, "gg");
    explorer.update(FilesUpdate::Root {
        path: "/".into(),
        name: "/".into(),
    });
    listed(&mut explorer, "/", vec![dir("etc"), dir("p")]);
    assert!(!explorer.on_up());
    assert_eq!(at(&mut explorer), "/etc");
    // A lesson's explorer, kept in its folder, has none at the fence…
    let mut explorer = super::tests::explorer(10);
    rooted(&mut explorer, project());
    explorer.fence("/p".into());
    assert_eq!(shown(&mut explorer)[1], "  ▸ src");
    // …but has one below it, which goes up to it.
    let mut explorer = super::tests::explorer(10);
    rooted_deeper(&mut explorer, project());
    explorer.fence("/home/me".into());
    assert_eq!(shown(&mut explorer)[1], "    ..");
    keys(&mut explorer, "gg<CR>");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Root("/home/me".into())]
    );
}

#[test]
fn dot_dot_shows_before_the_root_is_listed_and_nothing_above_is_listed() {
    let mut explorer = explorer(10);
    explorer.update(FilesUpdate::Root {
        path: "/home/me/p".into(),
        name: "~/p".into(),
    });
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Expand("/home/me/p".into())],
        "nothing of /home/me"
    );
    assert_eq!(shown(&mut explorer), ["~/p", "    .."]);
    // The only line, the cursor's on it, though a view kept of it now
    // doesn't say so.
    assert!(explorer.on_up());
    assert!(!explorer.view().up);
    // Listed once it comes, the cursor goes to the first entry.
    listed(&mut explorer, "/home/me/p", project());
    assert_eq!(at(&mut explorer), "/home/me/p/src");
}

#[test]
fn a_root_that_cant_be_listed_isnt_asked_for_again_and_dot_dot_leads_out() {
    let mut explorer = explorer(10);
    rooted_deeper(&mut explorer, project());
    let unlisted = || FilesUpdate::Unlisted {
        dir: "/home/me/p".into(),
        reason: "Permission denied".into(),
    };
    explorer.update(unlisted());
    assert_eq!(explorer.label(), "~/p: Permission denied");
    assert_eq!(explorer.take_outbox(), [], "not asked for again and again");
    assert_eq!(shown(&mut explorer), ["~/p", "    .."]);
    // R tries again, once.
    keys(&mut explorer, "R");
    assert_eq!(
        explorer.take_outbox(),
        [
            FilesAction::Expand("/home/me/p".into()),
            FilesAction::Refresh
        ]
    );
    explorer.update(unlisted());
    assert_eq!(explorer.take_outbox(), []);
    // Enter, with the cursor on `..`, the only line, goes up, and the new
    // root is asked for.
    keys(&mut explorer, "<CR>");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Root("/home/me".into())]
    );
    explorer.update(FilesUpdate::Root {
        path: "/home/me".into(),
        name: "~".into(),
    });
    // The old root, open in it, is tried once more, as a folder: one that
    // can't be listed is collapsed, and so not asked for again.
    assert_eq!(
        explorer.take_outbox(),
        [
            FilesAction::Expand("/home/me".into()),
            FilesAction::Expand("/home/me/p".into())
        ]
    );
}

#[test]
fn in_an_empty_folder_the_cursor_is_on_dot_dot_and_a_makes_a_file_in_the_folder() {
    let mut explorer = explorer(10);
    rooted_deeper(&mut explorer, vec![file(".hidden")]);
    assert!(explorer.on_up(), "the only line shown");
    explorer.settle();
    assert_eq!(explorer.scene().1.map(|cursor| cursor.line), Some(1));
    keys(&mut explorer, "a");
    assert_eq!(
        explorer.label(),
        "new file in ~/p/ (end with / for a directory): ▏"
    );
    keys(&mut explorer, "<Esc>");
    // A file made there shows, and the cursor, not put on `..`, goes to
    // it.
    let made = vec![file(".hidden"), file("n.txt")];
    listed(&mut explorer, "/home/me/p", made.clone());
    assert_eq!(at(&mut explorer), "/home/me/p/n.txt");
    // Put on `..`, it stays there.
    keys(&mut explorer, "k");
    listed(&mut explorer, "/home/me/p", made);
    assert!(explorer.on_up());
    keys(&mut explorer, "<CR>");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Root("/home/me".into())]
    );
}

#[test]
fn gone_up_from_a_hidden_folder_the_cursor_goes_to_the_first_entry() {
    for typed in ["gg<CR>", "-"] {
        let mut explorer = explorer(10);
        explorer.update(FilesUpdate::Root {
            path: "/home/me/.config".into(),
            name: "~/.config".into(),
        });
        explorer.take_outbox();
        listed(&mut explorer, "/home/me/.config", vec![dir("nvim")]);
        keys(&mut explorer, typed);
        assert_eq!(
            explorer.take_outbox(),
            [FilesAction::Root("/home/me".into())],
            "{typed}"
        );
        explorer.update(FilesUpdate::Root {
            path: "/home/me".into(),
            name: "~".into(),
        });
        listed(&mut explorer, "/home/me", vec![dir(".config"), dir("p")]);
        assert!(!explorer.on_up(), "{typed}");
        assert_eq!(at(&mut explorer), "/home/me/p", "{typed}");
    }
}

#[test]
fn nothing_that_acts_on_a_file_acts_on_dot_dot() {
    let mut explorer = explorer(10);
    rooted_deeper(&mut explorer, project());
    keys(&mut explorer, "gg");
    // Refused, saying why: rename, delete, cut, copy, and opening in a
    // split or a tab.
    for refused in ["r", "d", "x", "y", "<C-v>", "<C-s>", "<C-t>"] {
        keys(&mut explorer, refused);
        assert!(!explorer.prompting(), "{refused}");
        assert_eq!(explorer.label(), ONLY_UP, "{refused}");
        assert_eq!(explorer.take_asks(), [], "{refused}");
        assert_eq!(explorer.take_outbox(), [], "{refused}");
        assert!(explorer.on_up(), "{refused}");
    }
    // Passed over, as a file's line is: folding, h, and zR, which opens
    // only the folders shown below it.
    keys(&mut explorer, "zozczahzM");
    assert_eq!(explorer.take_outbox(), []);
    assert!(explorer.on_up());
    keys(&mut explorer, "zR");
    assert_eq!(
        explorer.take_outbox(),
        [
            FilesAction::Expand("/home/me/p/src".into()),
            FilesAction::Expand("/home/me/p/target".into())
        ]
    );
    keys(&mut explorer, "zM");
    explorer.take_outbox();
    // What goes into a folder goes into the root, as with no line at the
    // cursor, never into the folder above: a, p and e.
    keys(&mut explorer, "a");
    assert_eq!(
        explorer.label(),
        "new file in ~/p/ (end with / for a directory): ▏"
    );
    keys(&mut explorer, "n.txt<CR>");
    assert_eq!(
        explorer.take_asks(),
        [Ask::Op(FileOp::Create {
            path: "/home/me/p/n.txt".into(),
            dir: false
        })]
    );
    keys(&mut explorer, "p");
    assert_eq!(
        explorer.take_asks(),
        [Ask::Paste {
            dir: "/home/me/p".into()
        }]
    );
    keys(&mut explorer, "e");
    let asks = explorer.take_asks();
    let [
        Ask::Edit {
            text: Some(text),
            at: None,
            ..
        },
    ] = &asks[..]
    else {
        panic!("not the root's listing: {asks:?}");
    };
    assert_eq!(explorer.folder_shown(), Some("/home/me/p"));
    // The listing starts with `..`, which has no mark, so it names
    // nothing: gone, moved, put again or indented, it comes to nothing.
    let names: Vec<&str> = (text.lines())
        .map(|line| &line[listing::mark_len(line)..])
        .collect();
    assert_eq!(names, ["..", "src/", "target/", "Cargo.toml"]);
    let entries = text.strip_prefix("..\n").expect("`..` first");
    for (before, after) in [
        ("", ""),
        ("", "..\n"),
        ("../\n..\n", "\t..\n"),
        ("  ../ \n", ""),
    ] {
        let planned = explorer.plan(&[&format!("{before}{entries}{after}")]);
        assert_eq!(planned, Planned::Nothing, "{before:?} {after:?}");
    }
    // An entry's line renamed `..` would name the folder above: refused.
    let src = text.lines().nth(1).expect("src's line");
    let renamed = text.replace(src, &format!("{}..", &src[..listing::mark_len(src)]));
    let planned = explorer.plan(&[&renamed]);
    assert!(
        matches!(&planned, Planned::Refused { at: Some((0, 1)), why } if why.contains("holds it")),
        "{planned:?}"
    );
}

#[test]
fn a_search_passes_over_dot_dot() {
    let mut explorer = explorer(10);
    rooted_deeper(&mut explorer, vec![file("a.rs"), file("b.rs")]);
    keys(&mut explorer, "gg/.<CR>");
    assert_eq!(at(&mut explorer), "/home/me/p/a.rs");
    keys(&mut explorer, "N");
    assert_eq!(
        at(&mut explorer),
        "/home/me/p/b.rs",
        "round the end, past .."
    );
    keys(&mut explorer, "n");
    assert_eq!(at(&mut explorer), "/home/me/p/a.rs");
}

#[test]
fn an_explorer_kept_shows_its_cursor_where_it_was_on_a_file_or_on_dot_dot() {
    let opened = |view: View, reveal: Option<&str>| {
        let mut again = Explorer::new(FilesId(2), &Options::default());
        again.resize(Size::new(40, 10).unwrap());
        if let Some(path) = reveal {
            again.reveal(path.into());
        }
        again.update(FilesUpdate::Root {
            path: "/home/me/p".into(),
            name: "~/p".into(),
        });
        again.restore(view);
        listed(&mut again, "/home/me/p", project());
        again
    };
    let mut explorer = explorer(10);
    rooted_deeper(&mut explorer, project());
    // On a file: the same file, never the line before or after it.
    keys(&mut explorer, "G");
    let mut kept = opened(explorer.view(), None);
    assert_eq!(at(&mut kept), "/home/me/p/Cargo.toml");
    // On `..`: `..`.
    keys(&mut explorer, "gg");
    let mut kept = opened(explorer.view(), None);
    assert!(kept.on_up());
    keys(&mut kept, "j");
    assert_eq!(at(&mut kept), "/home/me/p/src");
    // Opened at a file to reveal, it goes there, not `..`, and stays.
    let mut revealing = opened(explorer.view(), Some("/home/me/p/Cargo.toml"));
    assert_eq!(at(&mut revealing), "/home/me/p/Cargo.toml");
    listed(&mut revealing, "/home/me/p", project());
    assert_eq!(at(&mut revealing), "/home/me/p/Cargo.toml");
}

/// The text of the listing `explorer` asks to show, and the mark of the
/// line the cursor goes to, if it says.
fn listing_asked(explorer: &mut Explorer) -> (String, Option<String>) {
    let asks = explorer.take_asks();
    match &asks[..] {
        [Ask::Edit { text, at, .. }] => (text.clone().unwrap_or_default(), at.clone()),
        _ => panic!("not a listing: {asks:?}"),
    }
}

#[test]
fn a_listing_starts_with_dot_dot_but_not_at_slash_nor_at_a_fence() {
    // At `/`, none.
    let mut explorer = explorer(10);
    explorer.update(FilesUpdate::Root {
        path: "/".into(),
        name: "/".into(),
    });
    listed(&mut explorer, "/", vec![dir("etc"), dir("p")]);
    keys(&mut explorer, "e");
    let (text, _) = listing_asked(&mut explorer);
    assert!(text.ends_with("p/\n") && !text.contains(".."), "{text:?}");
    // At a fence, none, but below it, one, first.
    let mut explorer = super::tests::explorer(10);
    rooted(&mut explorer, project());
    explorer.fence("/p".into());
    keys(&mut explorer, "e");
    let (text, _) = listing_asked(&mut explorer);
    assert!(!text.contains(".."), "{text:?}");
    // Shown once it's listed.
    explorer.edit_folder("/p/src".into(), None);
    assert_eq!(explorer.take_asks(), []);
    listed(&mut explorer, "/p/src", vec![file("main.rs")]);
    let (text, _) = listing_asked(&mut explorer);
    let names: Vec<&str> = (text.lines())
        .map(|line| &line[listing::mark_len(line)..])
        .collect();
    assert_eq!(names, ["..", "main.rs"]);
    // Enter on it, however it's written, shows the folder above's, with
    // the cursor on the folder come from.
    for typed in ["..", "  ../ "] {
        explorer.enter_line(typed);
        let (_, at) = listing_asked(&mut explorer);
        assert_eq!(explorer.folder_shown(), Some("/p"), "{typed:?}");
        let at = at.expect("a line to go to");
        assert_eq!(explorer.path_of_line(&at).as_deref(), Some("/p/src"));
        explorer.edit_folder("/p/src".into(), None);
        explorer.take_asks();
    }
    // At the fence, a `..` put there goes nowhere.
    explorer.edit_folder("/p".into(), None);
    explorer.take_asks();
    explorer.enter_line("..");
    assert_eq!(explorer.take_asks(), []);
    assert_eq!(explorer.folder_shown(), Some("/p"));
}

#[test]
fn paths_are_said_from_the_root_only_if_theyre_in_it() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, project());
    assert_eq!(explorer.relative("/p"), "");
    assert_eq!(explorer.relative("/p/src/main.rs"), "src/main.rs");
    // A name that only starts as the root's does isn't in it.
    assert_eq!(explorer.relative("/px"), "/px");
    assert_eq!(explorer.relative("/p.old/a"), "/p.old/a");
    assert_eq!(explorer.relative("/"), "/");
    assert_eq!(explorer.shown("/px"), "/px");
    assert_eq!(explorer.shown("/p/src"), "~/p/src");
    // From `/`, everything is in it.
    let mut explorer = super::tests::explorer(10);
    explorer.update(FilesUpdate::Root {
        path: "/".into(),
        name: "/".into(),
    });
    assert_eq!(explorer.relative("/"), "");
    assert_eq!(explorer.relative("/etc/hosts"), "etc/hosts");
}

#[test]
fn an_explorer_takes_every_lists_move_and_its_keys_help_comes_from_its_dialect() {
    let mut explorer = explorer(11);
    let entries = (0..100).map(|n| file(&format!("f{n:03}"))).collect();
    rooted(&mut explorer, entries);
    keys(&mut explorer, "20G");
    shown(&mut explorer);
    keys(&mut explorer, "zt");
    shown(&mut explorer);
    keys(&mut explorer, "L");
    assert_eq!(at(&mut explorer), "/p/f027", "the view's bottom");
    keys(&mut explorer, "3H");
    assert_eq!(at(&mut explorer), "/p/f020");
    keys(&mut explorer, "M");
    assert_eq!(at(&mut explorer), "/p/f022");
    keys(&mut explorer, "<C-e><C-e>");
    assert_eq!(at(&mut explorer), "/p/f022");
    keys(&mut explorer, "5<C-n>");
    assert_eq!(at(&mut explorer), "/p/f027");
    // `/` with a count as big as there is goes round once, at once.
    keys(&mut explorer, "/f00[0-2]<CR>");
    assert_eq!(at(&mut explorer), "/p/f000");
    keys(&mut explorer, "99999999n");
    assert_eq!(at(&mut explorer), "/p/f000");
    keys(&mut explorer, "2n");
    assert_eq!(at(&mut explorer), "/p/f002");
    // A count and `z` half typed show in the bar; leaving forgets them.
    keys(&mut explorer, "2z");
    assert!(explorer.label().starts_with("2z  "), "{}", explorer.label());
    explorer.leave();
    assert!(!explorer.label().starts_with("2z"));
    let rows = explorer.shortcut_context().rows;
    let has = |key: &str, said: &str| rows.iter().any(|(k, d)| k == key && d == said);
    assert!(has("zo", "Expand directory"), "{rows:?}");
    assert!(has("zt", "Cursor line at top"));
    assert!(has("g.", "Show or hide hidden files"));
    assert!(has("Ctrl+V", "Open in right split"));
    assert!(has("H", "Top of view"));
}

#[test]
fn a_paste_goes_into_a_search_or_a_name_being_typed() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, vec![file("alpha.rs"), file("beta.md")]);
    assert!(!explorer.paste("x"), "nothing typed");
    keys(&mut explorer, "/");
    assert!(explorer.paste("beta"));
    keys(&mut explorer, "<CR>");
    assert_eq!(at(&mut explorer), "/p/beta.md");
}

#[test]
fn a_key_that_does_nothing_still_clears_a_message_and_redraws() {
    let mut explorer = explorer(10);
    rooted(&mut explorer, vec![file("alpha.rs")]);
    keys(&mut explorer, "/zzz<CR>");
    assert_eq!(explorer.label(), "no name shown matches zzz");
    assert!(explorer.stroke(Stroke::Escape), "drawn again");
    assert_ne!(explorer.label(), "no name shown matches zzz");
}
