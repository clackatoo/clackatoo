use std::fmt::Write;

use super::*;

fn file(name: &str) -> Entry {
    Entry {
        name: name.into(),
        kind: EntryKind::File { executable: false },
        ignored: false,
    }
}

fn dir(name: &str) -> Entry {
    Entry {
        name: name.into(),
        kind: EntryKind::Dir,
        ignored: false,
    }
}

/// A session of listings: the folders' entries, and every entry shown,
/// numbered in turn from the first folder's.
struct Session {
    folders: Vec<(String, Vec<Entry>)>,
    originals: Vec<Original>,
}

impl Session {
    /// The listings of `folders`, each with what's in it, every entry shown
    /// but those named in `hidden`.
    fn new(folders: &[(&str, Vec<Entry>)], hidden: &[&str]) -> Self {
        let mut originals = Vec::new();
        for (dir, entries) in folders {
            for entry in entries {
                if !hidden.contains(&entry.name.as_str()) {
                    originals.push(Original {
                        path: join(dir, &entry.name),
                        kind: entry.kind,
                    });
                }
            }
        }
        Self {
            folders: (folders.iter())
                .map(|(dir, entries)| ((*dir).to_owned(), entries.clone()))
                .collect(),
            originals,
        }
    }

    /// The line of the entry at `path`, as it was.
    fn line(&self, path: &str) -> String {
        let id = (self.originals.iter())
            .position(|original| original.path == path)
            .expect("an entry shown");
        let original = &self.originals[id];
        line_of(id, original.path.rsplit('/').next().unwrap(), original.kind)
    }

    /// The line of the entry at `path`, renamed `typed`.
    fn renamed(&self, path: &str, typed: &str) -> String {
        let id = (self.originals.iter())
            .position(|original| original.path == path)
            .expect("an entry shown");
        format!("{}{typed}", mark(id))
    }

    /// What the listings' texts, as edited to `texts`, come to.
    fn plan(&self, texts: &[&str]) -> Result<Vec<Step>, Problem> {
        let folders: Vec<Folder<'_>> = (self.folders.iter())
            .zip(texts)
            .map(|((dir, entries), text)| Folder { dir, entries, text })
            .collect();
        plan(&self.originals, &folders)
    }
}

/// `/p`, holding `src/`, `a.txt`, `b.txt` and `c.txt`.
fn p() -> Session {
    Session::new(
        &[(
            "/p",
            vec![dir("src"), file("a.txt"), file("b.txt"), file("c.txt")],
        )],
        &[],
    )
}

/// The lines of `/p` as they were, with `change` made to them.
fn edited(session: &Session, change: impl FnOnce(&mut Vec<String>)) -> String {
    let mut lines: Vec<String> = ["/p/src", "/p/a.txt", "/p/b.txt", "/p/c.txt"]
        .iter()
        .map(|path| session.line(path))
        .collect();
    change(&mut lines);
    lines.join("\n") + "\n"
}

fn moved(from: &str, to: &str) -> Step {
    Step::Move {
        from: from.into(),
        to: to.into(),
        aside: false,
    }
}

fn aside(from: &str, to: &str) -> Step {
    Step::Move {
        from: from.into(),
        to: to.into(),
        aside: true,
    }
}

fn deleted(path: &str) -> Step {
    Step::Delete {
        path: path.into(),
        kind: EntryKind::File { executable: false },
    }
}

fn created(path: &str, dir: bool) -> Step {
    Step::Create {
        path: path.into(),
        dir,
    }
}

fn copied(from: &str, to: &str) -> Step {
    Step::Copy {
        from: from.into(),
        to: to.into(),
    }
}

/// Why `texts` can't be done, and on which line of which listing.
fn refused(session: &Session, texts: &[&str]) -> (Option<(usize, u64)>, String) {
    let problem = session.plan(texts).expect_err("refused");
    (problem.at, problem.why)
}

#[test]
fn marks_are_hidden_characters_read_back_whole_or_not_at_all() {
    for id in [0, 1, 15, 16, 255, 100_000, usize::MAX >> 16] {
        let line = line_of(id, "name.txt", EntryKind::File { executable: false });
        assert_eq!(read_mark(&line), Some((id, "name.txt")));
        assert_eq!(&line[mark_len(&line)..], "name.txt");
        assert!(!line.is_ascii() && line.chars().take_while(|&ch| in_marks(ch)).count() > 2);
    }
    assert_eq!(
        line_of(3, "src", EntryKind::Dir)
            .split_at(mark_len(&mark(3)))
            .1,
        "src/"
    );
    // Not a mark: nothing hidden.
    assert_eq!(mark_len("plain.txt"), 0);
    let whole = mark(42);
    let damaged = &whole[..whole.len() - MARK_END.len_utf8()];
    assert_eq!(mark_len(&format!("{damaged}x")), 0);
    assert_eq!(
        mark_len(&format!("{MARK_START}{MARK_END}x")),
        0,
        "no digits"
    );
}

#[test]
fn nothing_changed_is_nothing_to_do() {
    let session = p();
    assert_eq!(session.plan(&[&edited(&session, |_| {})]), Ok(Vec::new()));
    // Trailing spaces, an indent, a folder without its `/`, and blank
    // lines change nothing either.
    let text = edited(&session, |lines| {
        lines[0] = session.renamed("/p/src", "src");
        lines[1] = format!("  {}  ", lines[1]);
        lines.push(String::new());
        lines.insert(0, "   ".to_owned());
    });
    assert_eq!(session.plan(&[&text]), Ok(Vec::new()));
}

#[test]
fn a_line_changed_is_a_rename_and_a_path_a_move() {
    let session = p();
    let text = edited(&session, |lines| {
        lines[1] = session.renamed("/p/a.txt", "z.txt");
        lines[2] = session.renamed("/p/b.txt", "src/b.txt");
        lines[3] = session.renamed("/p/c.txt", "../c.txt");
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            moved("/p/a.txt", "/p/z.txt"),
            moved("/p/b.txt", "/p/src/b.txt"),
            moved("/p/c.txt", "/c.txt"),
        ])
    );
    let text = edited(&session, |lines| {
        lines[1] = session.renamed("/p/a.txt", "/tmp/./x/../a.txt");
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![moved("/p/a.txt", "/tmp/a.txt")])
    );
}

#[test]
fn a_line_gone_is_a_delete_and_a_new_line_a_new_file_or_folder() {
    let session = p();
    let text = edited(&session, |lines| {
        lines.remove(2);
        lines.push("notes.md".to_owned());
        lines.push("docs/".to_owned());
        lines.push("deep/er/file.rs".to_owned());
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            created("/p/notes.md", false),
            created("/p/docs", true),
            created("/p/deep/er/file.rs", false),
            deleted("/p/b.txt"),
        ])
    );
    // A folder deleted says so, for the confirmation to say what goes
    // with it.
    let text = edited(&session, |lines| {
        lines.remove(0);
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![Step::Delete {
            path: "/p/src".into(),
            kind: EntryKind::Dir
        }])
    );
}

#[test]
fn a_mix_goes_copies_then_new_then_moves_then_deletes() {
    let session = p();
    let text = edited(&session, |lines| {
        lines.remove(3);
        lines[1] = session.renamed("/p/a.txt", "src/a.txt");
        lines.push(session.renamed("/p/b.txt", "b copy.txt"));
        lines.insert(0, "new/".to_owned());
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            copied("/p/b.txt", "/p/b copy.txt"),
            created("/p/new", true),
            moved("/p/a.txt", "/p/src/a.txt"),
            deleted("/p/c.txt"),
        ])
    );
}

#[test]
fn names_swapped_or_in_a_ring_go_aside_first() {
    let session = p();
    let text = edited(&session, |lines| {
        lines[1] = session.renamed("/p/a.txt", "b.txt");
        lines[2] = session.renamed("/p/b.txt", "a.txt");
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            aside("/p/a.txt", "/p/a.txt.moving"),
            moved("/p/b.txt", "/p/a.txt"),
            moved("/p/a.txt.moving", "/p/b.txt"),
        ])
    );
    let text = edited(&session, |lines| {
        lines[1] = session.renamed("/p/a.txt", "b.txt");
        lines[2] = session.renamed("/p/b.txt", "c.txt");
        lines[3] = session.renamed("/p/c.txt", "a.txt");
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            aside("/p/a.txt", "/p/a.txt.moving"),
            moved("/p/c.txt", "/p/a.txt"),
            moved("/p/b.txt", "/p/c.txt"),
            moved("/p/a.txt.moving", "/p/b.txt"),
        ])
    );
    // A name to go aside to that's taken, shown or not, isn't used.
    let session = Session::new(
        &[(
            "/p",
            vec![file("a"), file("a.moving"), file("b"), file(".a.moving-2")],
        )],
        &["a.moving"],
    );
    let text = format!(
        "{}\n{}\n{}\n",
        session.renamed("/p/a", "b"),
        session.renamed("/p/b", "a"),
        session.renamed("/p/.a.moving-2", "a.moving-2"),
    );
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            moved("/p/.a.moving-2", "/p/a.moving-2"),
            aside("/p/a", "/p/a.moving-3"),
            moved("/p/b", "/p/a"),
            moved("/p/a.moving-3", "/p/b"),
        ])
    );
}

#[test]
fn a_name_freed_is_taken_only_once_its_free() {
    let session = p();
    // b.txt deleted, and a.txt renamed to it: the delete goes first.
    let text = edited(&session, |lines| {
        lines.remove(2);
        lines[1] = session.renamed("/p/a.txt", "b.txt");
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![deleted("/p/b.txt"), moved("/p/a.txt", "/p/b.txt")])
    );
    // A folder renamed, and a new file in it by its new name: the new
    // file goes in once the folder's there.
    let text = edited(&session, |lines| {
        lines[0] = session.renamed("/p/src", "lib/");
        lines.push("lib/new.rs".to_owned());
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            moved("/p/src", "/p/lib"),
            created("/p/lib/new.rs", false)
        ])
    );
}

#[test]
fn a_line_twice_is_a_copy_and_twice_unchanged_is_refused() {
    let session = p();
    let text = edited(&session, |lines| {
        lines.push(session.renamed("/p/a.txt", "a2.txt"));
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![copied("/p/a.txt", "/p/a2.txt")])
    );
    // Neither where it was: the first moves, the rest copy, first.
    let text = edited(&session, |lines| {
        lines[1] = session.renamed("/p/a.txt", "x.txt");
        lines.push(session.renamed("/p/a.txt", "y.txt"));
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            copied("/p/a.txt", "/p/y.txt"),
            moved("/p/a.txt", "/p/x.txt")
        ])
    );
    let text = edited(&session, |lines| {
        let line = lines[1].clone();
        lines.insert(2, line);
    });
    assert_eq!(
        refused(&session, &[&text]),
        (
            Some((0, 2)),
            "two lines are both a.txt: a copy needs a name of its own".into()
        )
    );
}

#[test]
fn a_name_already_there_is_refused_even_if_its_hidden() {
    let session = p();
    let text = edited(&session, |lines| lines.push("b.txt".to_owned()));
    assert_eq!(
        refused(&session, &[&text]),
        (Some((0, 4)), "b.txt is already there".into())
    );
    let text = edited(&session, |lines| {
        lines[1] = session.renamed("/p/a.txt", "c.txt");
    });
    assert_eq!(
        refused(&session, &[&text]),
        (Some((0, 1)), "c.txt is already there".into())
    );
    let text = edited(&session, |lines| {
        lines.push("x".to_owned());
        lines.push("x".to_owned());
    });
    assert_eq!(
        refused(&session, &[&text]),
        (Some((0, 5)), "two lines are both x".into())
    );
    let session = Session::new(&[("/p", vec![file(".env"), file("a")])], &[".env"]);
    let text = format!("{}\n", session.renamed("/p/a", ".env"));
    assert_eq!(
        refused(&session, &[&text]),
        (Some((0, 0)), ".env is already there".into())
    );
}

#[test]
fn nothing_goes_into_a_file_or_into_what_moves_or_is_deleted() {
    let session = p();
    let text = edited(&session, |lines| lines.push("a.txt/x".to_owned()));
    assert_eq!(
        refused(&session, &[&text]),
        (Some((0, 4)), "a.txt is a file, not a folder".into())
    );
    let text = edited(&session, |lines| {
        lines.push("new".to_owned());
        lines.push("new/x".to_owned());
    });
    assert_eq!(
        refused(&session, &[&text]),
        (Some((0, 5)), "new is a file, not a folder".into())
    );
    let text = edited(&session, |lines| {
        lines.remove(0);
        lines.push("src/x".to_owned());
    });
    let (at, why) = refused(&session, &[&text]);
    assert_eq!(at, Some((0, 3)));
    assert!(why.starts_with("src is being moved or deleted"), "{why}");
    // Into itself.
    let text = edited(&session, |lines| {
        lines[0] = session.renamed("/p/src", "src/inner/");
    });
    let (at, why) = refused(&session, &[&text]);
    assert_eq!(at, Some((0, 0)));
    assert!(why.starts_with("src is being moved or deleted"), "{why}");
    // Where the folder itself is.
    let text = edited(&session, |lines| {
        lines[1] = session.renamed("/p/a.txt", "./");
    });
    assert_eq!(
        refused(&session, &[&text]),
        (
            Some((0, 1)),
            "./ is where this listing's folder is, or holds it".into()
        )
    );
}

#[test]
fn a_damaged_or_strange_mark_is_refused_never_read_as_another() {
    let session = p();
    // Two lines joined, as `J` joins them.
    let text = edited(&session, |lines| {
        let second = lines.remove(2);
        lines[1] = format!("{} {second}", lines[1]);
    });
    let (at, why) = refused(&session, &[&text]);
    assert_eq!(at, Some((0, 1)));
    assert!(
        why.starts_with("this line's hidden mark is damaged"),
        "{why}"
    );
    // Its last character taken away, as Backspace at the start of a name
    // might.
    let text = edited(&session, |lines| {
        let line = lines[3].replacen(MARK_END, "", 1);
        lines[3] = line;
    });
    assert_eq!(refused(&session, &[&text]).0, Some((0, 3)));
    // From a listing no longer open.
    let text = edited(&session, |lines| {
        lines.push(line_of(99, "x", EntryKind::Dir));
    });
    assert_eq!(
        refused(&session, &[&text]),
        (
            Some((0, 4)),
            "this line's hidden mark is from a listing no longer open".into()
        )
    );
}

#[test]
fn names_are_checked_before_anything_is_done() {
    let session = p();
    let cases: [(&str, &str); 4] = [
        (
            "",
            "this line has no name: delete the line to delete what it was",
        ),
        (
            "a.txt/",
            "a.txt is a file: a / at the end is for a new folder, on a line of its own",
        ),
        ("../../..", "../../.. goes above /"),
        ("x\0y", "a name can't hold a NUL"),
    ];
    for (typed, why) in cases {
        let text = edited(&session, |lines| {
            lines[1] = session.renamed("/p/a.txt", typed);
        });
        assert_eq!(
            refused(&session, &[&text]),
            (Some((0, 1)), why.into()),
            "{typed:?}"
        );
    }
    assert_eq!(
        refused(
            &session,
            &[&edited(&session, |lines| lines.push("/".into()))]
        ),
        (Some((0, 4)), "/ alone isn't a name".into())
    );
}

#[test]
fn a_line_cut_from_one_listing_and_put_in_another_is_a_move() {
    let session = Session::new(
        &[
            ("/p", vec![dir("src"), file("a.txt")]),
            ("/p/src", vec![file("main.rs")]),
        ],
        &[],
    );
    let p = session.line("/p/src");
    let src = format!(
        "{}\n{}\n",
        session.line("/p/src/main.rs"),
        session.line("/p/a.txt")
    );
    assert_eq!(
        session.plan(&[&p, &src]),
        Ok(vec![moved("/p/a.txt", "/p/src/a.txt")])
    );
    // Out of a folder that's then deleted: the move goes first.
    let p = format!(
        "{}\n{}\n",
        session.line("/p/a.txt"),
        session.line("/p/src/main.rs")
    );
    assert_eq!(
        session.plan(&[&p, ""]),
        Ok(vec![
            moved("/p/src/main.rs", "/p/main.rs"),
            Step::Delete {
                path: "/p/src".into(),
                kind: EntryKind::Dir
            },
        ])
    );
    // Copied into the other, the original staying.
    let src = format!(
        "{}\n{}\n",
        session.line("/p/src/main.rs"),
        session.line("/p/a.txt")
    );
    let p = format!("{}\n{}\n", session.line("/p/src"), session.line("/p/a.txt"));
    assert_eq!(
        session.plan(&[&p, &src]),
        Ok(vec![copied("/p/a.txt", "/p/src/a.txt")])
    );
}

#[test]
fn a_hundred_thousand_renames_are_planned_in_a_moment() {
    let entries: Vec<Entry> = (0..100_000).map(|n| file(&format!("f{n}"))).collect();
    let session = Session::new(&[("/p", entries)], &[]);
    // Every name moved along one, round in a ring: the worst case.
    let mut text = String::new();
    for n in 0..100_000 {
        writeln!(text, "{}f{}", mark(n), (n + 1) % 100_000).unwrap();
    }
    let started = std::time::Instant::now();
    let steps = session.plan(&[&text]).unwrap();
    assert_eq!(steps.len(), 100_001, "one goes aside");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
