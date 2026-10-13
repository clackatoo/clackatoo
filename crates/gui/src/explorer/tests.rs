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
    explorer.lines[explorer.cursor].path()
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
        ["~/p", "  ▸ src", "  ▸ target", "    Cargo.toml"],
        "hidden files hidden"
    );
    keys(&mut explorer, "l");
    assert_eq!(
        explorer.take_outbox(),
        [FilesAction::Expand("/p/src".into())]
    );
    assert_eq!(
        shown(&mut explorer)[1],
        "  ▾ src",
        "expanded before it's listed"
    );
    listed(&mut explorer, "/p/src", vec![dir("bin"), file("main.rs")]);
    assert_eq!(
        shown(&mut explorer),
        [
            "~/p",
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
    assert_eq!(shown(&mut explorer).len(), 4);
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
    assert_eq!(shown(&mut explorer).len(), 4);
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
    keys(&mut explorer, "gg");
    assert_eq!(at(&mut explorer), "/p/f000");
    keys(&mut explorer, "12G");
    assert_eq!(at(&mut explorer), "/p/f011");
    keys(&mut explorer, "3gg2k");
    assert_eq!(at(&mut explorer), "/p/f000");
    keys(&mut explorer, "<C-d>");
    assert_eq!(at(&mut explorer), "/p/f005", "half of the ten lines shown");
    keys(&mut explorer, "<C-u>");
    assert_eq!(at(&mut explorer), "/p/f000");
    keys(&mut explorer, "<C-f>");
    assert_eq!(at(&mut explorer), "/p/f008");
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
    let matches: Vec<usize> = (explorer.highlights().iter())
        .filter(|highlight| highlight.kind == HighlightKind::Match)
        .map(|highlight| highlight.line)
        .collect();
    assert_eq!(matches, [1, 3, 4]);
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
        style(rows[2], 0).fg,
        colors::git_mark(GitMark::Modified).1.fg
    );
    assert!(
        style(rows[5], 4).attributes.contains(Attributes::DIM),
        "ignored"
    );
    assert!(!style(rows[4], 4).attributes.contains(Attributes::DIM));
    // In a directory git says is untracked whole.
    keys(&mut explorer, "l");
    listed(&mut explorer, "/p/new", vec![file("n.rs")]);
    assert_eq!(shown(&mut explorer)[2], "? │   n.rs");
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
        shown(&mut explorer)[1],
        "  ▸ .git",
        "dimmed, as git ignores it"
    );
    keys(&mut explorer, "g.");
    assert_eq!(shown(&mut explorer)[1], "  ▸ src");

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
    assert_eq!(&shown(&mut explorer)[..3], ["/", "  ▸ etc", "  ▾ p"]);
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
    assert_eq!(shown(&mut explorer)[1], "  ▸ src");
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
    assert!(explorer.click(3, false));
    assert_eq!(at(&mut explorer), "/p/f02");
    assert!(!explorer.click(0, false), "the root's line");
    explorer.click(2, true);
    let open = Ask::Open {
        path: "/p/f01".into(),
        how: Opening::Here,
    };
    assert_eq!(explorer.take_asks(), [open]);
    assert!(explorer.scroll(10));
    assert_eq!(at(&mut explorer), "/p/f10", "brought into view");
    assert_eq!(shown(&mut explorer)[1], "    f10");
    explorer.scroll(-100);
    assert_eq!(shown(&mut explorer)[1], "    f00");
    assert_eq!(at(&mut explorer), "/p/f04");
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
