use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

use super::*;

/// The editing session the listings' marks are of.
const SESSION: u32 = 0xc1ac_a700;

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
                        stamp: None,
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
        line_of(
            SESSION,
            id,
            original.path.rsplit('/').next().unwrap(),
            original.kind,
        )
    }

    /// The line of the entry at `path`, renamed `typed`.
    fn renamed(&self, path: &str, typed: &str) -> String {
        let id = (self.originals.iter())
            .position(|original| original.path == path)
            .expect("an entry shown");
        format!("{}{typed}", mark(SESSION, id))
    }

    /// What the listings' texts, as edited to `texts`, come to.
    /// It fails, rather than hangs, if planning goes on for ever.
    fn plan(&self, texts: &[&str]) -> Result<Vec<Step>, Problem> {
        let folders = self.folders.clone();
        let originals = self.originals.clone();
        let texts: Vec<String> = texts.iter().map(|text| (*text).to_owned()).collect();
        let (send, planned) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let folders: Vec<Folder<'_>> = (folders.iter())
                .zip(&texts)
                .map(|((dir, entries), text)| Folder { dir, entries, text })
                .collect();
            let _ = send.send(plan(SESSION, &originals, &folders));
        });
        planned
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("planning to finish")
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
        let line = line_of(
            SESSION,
            id,
            "name.txt",
            EntryKind::File { executable: false },
        );
        assert_eq!(read_mark(&line), Some((SESSION, id, "name.txt")));
        assert_eq!(id_of(SESSION, &line), Some(id));
        assert_eq!(id_of(SESSION + 1, &line), None, "another session's");
        assert_eq!(&line[mark_len(&line)..], "name.txt");
        assert!(!line.is_ascii() && line.chars().take_while(|&ch| in_marks(ch)).count() > 2);
    }
    assert_eq!(
        line_of(SESSION, 3, "src", EntryKind::Dir)
            .split_at(mark_len(&mark(SESSION, 3)))
            .1,
        "src/"
    );
    // Not a mark: nothing hidden.
    assert_eq!(mark_len("plain.txt"), 0);
    let whole = mark(SESSION, 42);
    let damaged = &whole[..whole.len() - MARK_END.len_utf8()];
    assert_eq!(mark_len(&format!("{damaged}x")), 0);
    assert_eq!(
        mark_len(&format!("{MARK_START}{MARK_SPLIT}{MARK_END}x")),
        0,
        "no digits"
    );
}

#[test]
fn a_line_from_another_sessions_listing_is_refused() {
    let session = p();
    let text = edited(&session, |lines| {
        lines.push(line_of(
            SESSION + 1,
            1,
            "a.txt",
            EntryKind::File { executable: false },
        ));
    });
    assert_eq!(
        refused(&session, &[&text]),
        (
            Some((0, 4)),
            "this line came from another listing: type its name on a new line instead".into()
        )
    );
}

#[test]
fn unwrapping_a_folder_in_a_folder_of_its_name_goes_aside_out_of_it() {
    // `proj/proj/`: the inner one's line cut and put in the outer's
    // folder, where the outer one's line is deleted.
    let session = Session::new(
        &[("/x", vec![dir("proj")]), ("/x/proj", vec![dir("proj")])],
        &[],
    );
    let outer = String::new();
    let inner_line = session.line("/x/proj/proj");
    let text = format!("{inner_line}\n");
    assert_eq!(
        session.plan(&[&text, &outer]),
        Ok(vec![
            aside("/x/proj/proj", "/x/proj.moving"),
            Step::Delete {
                path: "/x/proj".into(),
                kind: EntryKind::Dir
            },
            moved("/x/proj.moving", "/x/proj"),
        ])
    );
}

#[test]
fn moving_into_where_its_own_folder_moves_is_refused_not_looped_on() {
    // `src/` renamed `tmp/`, and `src/f` moved to `../tmp/f`.
    let session = Session::new(
        &[("/p", vec![dir("src")]), ("/p/src", vec![file("f")])],
        &[],
    );
    let p = format!("{}\n", session.renamed("/p/src", "tmp/"));
    let src = format!("{}\n", session.renamed("/p/src/f", "../tmp/f"));
    assert_eq!(
        refused(&session, &[&p, &src]),
        (
            Some((1, 0)),
            "../tmp/f goes along with src, which moves to tmp: leave its line as it was".into()
        )
    );
}

#[test]
fn a_folder_deleted_with_lines_of_its_own_still_staying_is_refused() {
    let session = Session::new(
        &[("/p", vec![dir("src")]), ("/p/src", vec![file("f")])],
        &[],
    );
    let src = format!("{}\n", session.line("/p/src/f"));
    assert_eq!(
        refused(&session, &["", &src]),
        (
            Some((1, 0)),
            "src is being deleted, but this line keeps f in it: delete the line too, or keep src"
                .into()
        )
    );
    // With its lines gone too, it goes, after what was in it.
    assert_eq!(
        session.plan(&["", ""]),
        Ok(vec![
            deleted("/p/src/f"),
            Step::Delete {
                path: "/p/src".into(),
                kind: EntryKind::Dir
            }
        ])
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
fn a_name_to_go_aside_to_isnt_one_a_line_makes_a_folder_of() {
    let session = Session::new(&[("/p", vec![file("a"), file("b")])], &[]);
    // `a.moving` is made, as the folder `a.moving/x` is in.
    let text = format!(
        "{}\n{}\na.moving/x\n",
        session.renamed("/p/a", "b"),
        session.renamed("/p/b", "a")
    );
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            created("/p/a.moving/x", false),
            aside("/p/a", "/p/a.moving-2"),
            moved("/p/b", "/p/a"),
            moved("/p/a.moving-2", "/p/b"),
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
    // A folder renamed, and a new file in it by its new name: what comes
    // with the folder may already have the name, so it waits for a write
    // of its own.
    let text = edited(&session, |lines| {
        lines[0] = session.renamed("/p/src", "lib/");
        lines.push("lib/new.rs".to_owned());
    });
    assert_eq!(
        refused(&session, &[&text]),
        (
            Some((0, 4)),
            "lib/new.rs would go in lib, which src is going to: put it there in a write of its \
             own"
            .into()
        )
    );
    // A new folder, and what's put in it, go in that order.
    let text = edited(&session, |lines| {
        lines.push("lib/".to_owned());
        lines[1] = session.renamed("/p/a.txt", "lib/a.txt");
    });
    assert_eq!(
        session.plan(&[&text]),
        Ok(vec![
            created("/p/lib", true),
            moved("/p/a.txt", "/p/lib/a.txt")
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
        lines.push(line_of(SESSION, 99, "x", EntryKind::Dir));
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
        writeln!(text, "{}f{}", mark(SESSION, n), (n + 1) % 100_000).unwrap();
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

/// Files and folders, in memory, for steps to be done to as the daemon
/// does them: each thing has a number, which moves with it, so what's
/// lost can be told.
#[derive(Debug, Default)]
struct Disk {
    things: BTreeMap<String, (bool, u64)>,
    trash: Vec<u64>,
    next: u64,
}

impl Disk {
    fn add(&mut self, path: &str, dir: bool) -> u64 {
        self.next += 1;
        self.things.insert(path.to_owned(), (dir, self.next));
        self.next
    }

    fn has(&self, path: &str) -> bool {
        path == "/" || self.things.contains_key(path)
    }

    /// What's at `path` and in it, if it's a folder.
    fn tree(&self, path: &str) -> Vec<String> {
        let within = format!("{path}/");
        (self.things.keys())
            .filter(|key| *key == path || key.starts_with(&within))
            .cloned()
            .collect()
    }

    /// Makes the folders `path` is in, as the daemon does.
    fn make_parents(&mut self, path: &str) -> Result<(), String> {
        let parent = parent_of(path);
        if self.has(parent) {
            return match self.things.get(parent) {
                Some((false, _)) => Err(format!("{parent} is a file")),
                _ => Ok(()),
            };
        }
        self.make_parents(parent)?;
        self.add(parent, true);
        Ok(())
    }

    /// Does `step`, refusing as the daemon would.
    fn apply(&mut self, step: &Step) -> Result<(), String> {
        let free = |disk: &Self, to: &str| {
            if disk.has(to) {
                Err(format!("{to} is already there"))
            } else {
                Ok(())
            }
        };
        match step {
            Step::Create { path, dir } => {
                free(self, path)?;
                self.make_parents(path)?;
                self.add(path, *dir);
            }
            Step::Copy { from, to } | Step::Move { from, to, .. } => {
                if !self.has(from) {
                    return Err(format!("{from} isn't there"));
                }
                free(self, to)?;
                if under(from, to) {
                    return Err(format!("{from} can't go inside itself"));
                }
                self.make_parents(to)?;
                let moving = matches!(step, Step::Move { .. });
                for path in self.tree(from) {
                    let moved = format!("{to}{}", &path[from.len()..]);
                    let (dir, id) = self.things[&path];
                    if moving {
                        self.things.remove(&path);
                        self.things.insert(moved, (dir, id));
                    } else {
                        self.add(&moved, dir);
                    }
                }
            }
            Step::Delete { path, .. } => {
                if !self.has(path) {
                    return Err(format!("{path} isn't there"));
                }
                for path in self.tree(path) {
                    let (_, id) = self.things.remove(&path).expect("there");
                    self.trash.push(id);
                }
            }
        }
        Ok(())
    }
}

/// Numbers that look random, the same each run.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % sides as u64).unwrap()
    }
}

/// A tree at random: /p, with a few of `names` in it, files and folders,
/// some holding more; on a disk in memory, and the folders with listings
/// of their own, with what's in them.
fn random_tree(dice: &mut Dice, names_had: &[&'static str]) -> (Disk, Vec<(String, Vec<Entry>)>) {
    let mut disk = Disk::default();
    disk.add("/p", true);
    let mut folders: Vec<(String, Vec<Entry>)> = vec![("/p".to_owned(), Vec::new())];
    let mut queue = vec![("/p".to_owned(), 0)];
    while let Some((parent, depth)) = queue.pop() {
        let mut names: Vec<&str> = names_had.to_vec();
        for _ in 0..dice.roll(4) + usize::from(depth == 0) {
            let name = names.remove(dice.roll(names.len()));
            let path = join(&parent, name);
            let is_dir = depth < 2 && dice.roll(2) == 0;
            disk.add(&path, is_dir);
            let entry = if is_dir { dir(name) } else { file(name) };
            let at = folders.iter().position(|(folder, _)| *folder == parent);
            if let Some(at) = at {
                folders[at].1.push(entry);
            }
            // Some folders have listings of their own.
            if is_dir && dice.roll(2) == 0 {
                folders.push((path.clone(), Vec::new()));
                queue.push((path, depth + 1));
            }
        }
    }
    (disk, folders)
}

/// The lines of `session`'s listings, of `folders`, edited at random,
/// with names from `names`.
fn random_edits(
    dice: &mut Dice,
    session: &Session,
    folders: &[(String, Vec<Entry>)],
    names: &[&str],
) -> Vec<Vec<String>> {
    let mut texts: Vec<Vec<String>> = (folders.iter())
        .map(|(dir, entries)| {
            (entries.iter())
                .map(|entry| session.line(&join(dir, &entry.name)))
                .collect()
        })
        .collect();
    let mut cut: Vec<String> = Vec::new();
    for text in &mut texts {
        for line in std::mem::take(text) {
            let name = names[dice.roll(names.len())];
            let mark = &line[..mark_len(&line)];
            match dice.roll(10) {
                0 => text.push(format!("{mark}{name}")),
                1 => {}
                2 => cut.push(line),
                3 => {
                    text.push(line.clone());
                    text.push(format!("{mark}{name}2"));
                }
                4 => text.push(format!("{mark}../{name}")),
                5 => text.push(format!("{mark}{name}/{name}")),
                6 => text.push(name.to_owned()),
                7 => {
                    text.push(line);
                    text.push(format!("{name}.moving/n"));
                }
                _ => text.push(line),
            }
        }
    }
    for line in cut {
        let at = dice.roll(texts.len());
        texts[at].push(line);
    }
    // Some listings have two names swapped, which goes aside, and a
    // line making a folder of the name one would go aside to.
    for text in &mut texts {
        let marked: Vec<usize> = (0..text.len())
            .filter(|&at| mark_len(&text[at]) > 0)
            .collect();
        if marked.len() < 2 || dice.roll(2) == 0 {
            continue;
        }
        let (one, two) = (marked[0], marked[1]);
        let split = |line: &str| line.split_at(mark_len(line)).1.to_owned();
        let (first, second) = (split(&text[one]), split(&text[two]));
        let first_mark = text[one][..mark_len(&text[one])].to_owned();
        let second_mark = text[two][..mark_len(&text[two])].to_owned();
        text[one] = format!("{first_mark}{second}");
        text[two] = format!("{second_mark}{first}");
        if dice.roll(2) == 0 {
            let name = first.trim_end_matches('/');
            text.push(format!("{name}.moving/n"));
        }
    }
    texts
}

#[test]
fn edits_at_random_are_planned_or_refused_and_planned_steps_lose_nothing() {
    // `x.moving` is what `x` would go aside to, and some lines make a
    // folder of it.
    const NAMES: [&str; 7] = ["a", "b", "c", "proj", "x", "x.moving", "d"];
    let mut dice = Dice(0x9e37_79b9_7f4a_7c15);
    let (mut planned, mut refused) = (0, 0);
    for _ in 0..3000 {
        let (mut disk, folders) = random_tree(&mut dice, &NAMES);
        let folders_shown: Vec<(&str, Vec<Entry>)> = (folders.iter())
            .map(|(dir, entries)| (dir.as_str(), entries.clone()))
            .collect();
        let session = Session::new(&folders_shown, &[]);
        let texts = random_edits(&mut dice, &session, &folders, &NAMES);
        let joined: Vec<String> = texts.iter().map(|lines| lines.join("\n")).collect();
        let joined: Vec<&str> = joined.iter().map(String::as_str).collect();
        let Ok(steps) = session.plan(&joined) else {
            refused += 1;
            continue;
        };
        planned += 1;
        let entries: usize = folders.iter().map(|(_, entries)| entries.len()).sum();
        let lines: usize = texts.iter().map(Vec::len).sum();
        assert!(steps.len() <= 2 * (entries + lines), "{steps:#?}");
        // What may go to the trash: what was in each thing deleted.
        let mut deletable = HashSet::new();
        for step in &steps {
            if let Step::Delete { path, .. } = step {
                deletable.extend(disk.tree(path).into_iter().map(|path| disk.things[&path].1));
            }
        }
        let before: Vec<u64> = disk.things.values().map(|&(_, id)| id).collect();
        let was: HashMap<String, u64> = (disk.things.iter())
            .map(|(path, &(_, id))| (path.clone(), id))
            .collect();
        for (number, step) in steps.iter().enumerate() {
            if let Err(why) = disk.apply(step) {
                panic!("step {number} failed: {why}\n{texts:#?}\n{steps:#?}");
            }
        }
        let after: HashSet<u64> = disk.things.values().map(|&(_, id)| id).collect();
        for id in before {
            let trashed = disk.trash.contains(&id);
            assert!(
                after.contains(&id) || trashed,
                "{id} lost\n{texts:#?}\n{steps:#?}"
            );
            assert!(
                !trashed || deletable.contains(&id),
                "{id} trashed unasked\n{steps:#?}"
            );
        }
        laid_out_as_the_lines_say(&session, &joined, &disk, &was);
    }
    assert!(
        planned > 300 && refused > 300,
        "{planned} planned, {refused} refused"
    );
}

/// Checks `disk`, which was as `was` says, has what `texts`, the
/// listings' lines, say: each line's file or folder at its path, the one
/// it was if it was moved, renamed or left as it was, and what was in a
/// folder that moved gone with it.
fn laid_out_as_the_lines_say(
    session: &Session,
    texts: &[&str],
    disk: &Disk,
    was: &HashMap<String, u64>,
) {
    let folders: Vec<Folder<'_>> = (session.folders.iter())
        .zip(texts)
        .map(|((dir, entries), text)| Folder { dir, entries, text })
        .collect();
    let lines = read_all(SESSION, &session.originals, &folders).expect("planned, so read");
    // The line each entry ends up at: the one keeping it, or else the
    // first to take it elsewhere.
    let mut keeper: HashMap<usize, usize> = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(id) = line.id {
            match keeper.get(&id) {
                Some(&kept) if lines[kept].stays || !line.stays => {}
                _ => {
                    keeper.insert(id, index);
                }
            }
        }
    }
    // Folders moved, from where they were to where they went.
    let moved: Vec<(&str, &str)> = (keeper.iter())
        .map(|(&id, &index)| (&session.originals[id], &lines[index]))
        .filter(|(original, line)| original.kind.expands() && original.path != line.path)
        .map(|(original, line)| (original.path.as_str(), line.path.as_str()))
        .collect();
    for (index, line) in lines.iter().enumerate() {
        // Gone with the innermost folder it was in that moved.
        let along = (moved.iter())
            .filter_map(|(from, to)| Some((from.len(), to, inside(from, &line.path)?)))
            .max_by_key(|&(depth, _, _)| depth);
        let path = match along {
            Some((_, to, rest)) => join(to, rest),
            None => line.path.clone(),
        };
        let Some(&(dir, id)) = disk.things.get(&path) else {
            panic!("{path} isn't there, as {:?} says\n{texts:#?}", line.typed);
        };
        assert_eq!(dir, line.dir, "{path}: a folder or not\n{texts:#?}");
        if let Some(entry) = line.id
            && keeper.get(&entry) == Some(&index)
        {
            let original = &session.originals[entry].path;
            assert_eq!(
                Some(&id),
                was.get(original),
                "{path} isn't {original}\n{texts:#?}"
            );
        }
    }
}
