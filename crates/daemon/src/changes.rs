//! Hunks as a review lists them, with the lines around them, and what's
//! changed in a work tree since its branch's base: the commit `git
//! merge-base` finds with `main`, `master` or the branch's upstream.
//!
//! Finding what's changed since the base runs git and reads files, so it
//! runs on a thread of its own, never the event loop. Files open in
//! buffers are compared as the buffers have them, changes not saved
//! included, and a hunk overlapping an agent's is marked as the agent's.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clackatoo_protocol::{BufferId, Made, ReviewFile, ReviewHunk, TerminalId};
use clackatoo_text::{AgentHunk, AgentHunks, Hunks, Rope};

use crate::buffer::{self, DiskStamp};
use crate::git::{git, git_filtering, output_fed, output_within, work_tree};

/// How many lines either side of a hunk a review shows.
pub(crate) const CONTEXT: usize = 3;

/// The most files a review of changes since the base lists: past them, a
/// branch is a rewrite, not something to read hunk by hunk.
const MOST_FILES: usize = 1000;

/// The biggest file compared with its base, in bytes.
const MOST_BYTES: usize = 4 * 1024 * 1024;

/// The most text compared, the files' at the base and now together, in
/// bytes: past it, a review of the base would take too long to work out
/// and to read.
const MOST_TOTAL: usize = 8 * 1024 * 1024;

/// What a review says when more has changed than that.
pub(crate) const TOO_MUCH: &str =
    "too much changed to show: narrow it with `:Review turn` or `chat`";

/// How long git is given for each thing asked of it.
const PATIENCE: Duration = Duration::from_secs(10);

/// What a thread comparing a work tree with its base knows of a file the
/// daemon has agent hunks for, or has open.
#[derive(Debug)]
pub(crate) struct Compared {
    /// Its real path.
    pub(crate) path: PathBuf,
    /// Its buffer and text, if it's open; otherwise its text is read.
    pub(crate) open: Option<(BufferId, Rope)>,
    /// Its agent hunks, in its text: for a file not open, the text it had
    /// when its stamp was `stamp`, and only then.
    pub(crate) agents: AgentHunks,
    pub(crate) stamp: Option<DiskStamp>,
}

/// `hunk`, in `text`, as a review lists it.
pub(crate) fn listed(hunk: &AgentHunk, text: &Rope) -> ReviewHunk {
    let lines = hunk.lines(text);
    let whole = if hunk.deleted() {
        let at = hunk.start.min(text.len_bytes());
        at..at
    } else {
        hunk.whole_lines(text)
    };
    let (before, after) = context(text, lines.start, lines.end);
    ReviewHunk {
        line: number(lines.start),
        before,
        base: hunk.base.clone(),
        text: text.byte_slice(whole).to_string(),
        after,
        agents: vec![hunk.id],
        by: Some(Made {
            chat: TerminalId(hunk.chat),
            turn: hunk.turn,
        }),
        reviewed: hunk.reviewed,
    }
}

/// The lines of `text` before line `first`, and from line `end`, as many
/// as a review shows, each without its line end.
pub(crate) fn context(text: &Rope, first: usize, end: usize) -> (Vec<String>, Vec<String>) {
    let line = |at: usize| {
        let line = text.line(at).to_string();
        line.strip_suffix('\n').unwrap_or(&line).to_owned()
    };
    let lines = text.len_lines();
    // The line after the last line end is a line only if it has text.
    let last = if text.line(lines - 1).len_bytes() == 0 {
        lines - 1
    } else {
        lines
    };
    let before = (first.saturating_sub(CONTEXT)..first.min(last))
        .map(line)
        .collect();
    let after = (end.min(last)..(end + CONTEXT).min(last))
        .map(line)
        .collect();
    (before, after)
}

/// Line `line`, counting from 0, as a review numbers it, from 1.
fn number(line: usize) -> u32 {
    u32::try_from(line + 1).unwrap_or(u32::MAX)
}

/// What's changed in the work tree `dir` is in since its branch's base,
/// file by file, with paths shown from `dir`, and each hunk marked with
/// the agent hunks of `known` it overlaps. Or why it can't be said.
pub(crate) fn changes_since_base(
    dir: &Path,
    known: &[Compared],
) -> Result<Vec<ReviewFile>, String> {
    let (top, _) =
        work_tree(dir).ok_or_else(|| git(dir).err().unwrap_or("this isn't in a git repository"))?;
    let top = top.canonicalize().unwrap_or(top);
    let base = merge_base(&top)?;
    let paths = changed_paths(&top, &base, known)?;
    let asked: Vec<u8> = (paths.iter())
        .flat_map(|(_, relative)| format!("{base}:{relative}\n").into_bytes())
        .collect();
    let checked = output_fed(
        git(&top)?.args(["cat-file", "--batch-check"]),
        asked,
        PATIENCE,
    )
    .ok_or("git couldn't say how big the files were at the base")?;
    let then_sizes: Vec<Option<(String, usize)>> = (String::from_utf8_lossy(&checked).lines())
        .map(|line| {
            let mut fields = line.split(' ');
            let (object, kind, size) = (fields.next()?, fields.next()?, fields.next()?);
            (kind == "blob").then(|| Some((object.to_owned(), size.parse().ok()?)))?
        })
        .collect();
    // Which to compare, and what's known of each: too big a file is left
    // out, and too much altogether is refused.
    let mut total = 0;
    let mut wanted: Vec<Option<Option<&Compared>>> = Vec::with_capacity(paths.len());
    for ((path, _), then) in paths.iter().zip(&then_sizes) {
        let compared = known.iter().find(|compared| &compared.path == path);
        let now = match compared.and_then(|known| known.open.as_ref()) {
            Some((_, text)) => text.len_bytes(),
            None => std::fs::metadata(path).map_or(0, |metadata| {
                usize::try_from(metadata.len()).unwrap_or(usize::MAX)
            }),
        };
        let then = then.as_ref().map_or(0, |(_, size)| *size);
        if then > MOST_BYTES || now > MOST_BYTES {
            wanted.push(None);
            continue;
        }
        total += then + now;
        if total > MOST_TOTAL {
            return Err(TOO_MUCH.to_owned());
        }
        wanted.push(Some(compared));
    }
    let objects: Vec<&str> = (then_sizes.iter().flatten())
        .map(|(object, size)| (object.as_str(), *size))
        .filter(|&(_, size)| size <= MOST_BYTES)
        .map(|(object, _)| object)
        .collect();
    let blobs = read_blobs(&top, &objects)?;
    let mut files = Vec::new();
    for (((path, _), then), wanted) in paths.iter().zip(&then_sizes).zip(wanted) {
        let Some(compared) = wanted else {
            continue;
        };
        let then = match then {
            Some((object, _)) => match blobs.get(object.as_str()) {
                Some(Some(text)) => text.clone(),
                // Not text, or too big.
                _ => continue,
            },
            None => String::new(),
        };
        let Some(file) = compare(&then, path, compared) else {
            continue;
        };
        let shown = path.strip_prefix(dir).unwrap_or(path).display().to_string();
        files.push(ReviewFile {
            path: path.display().to_string(),
            shown,
            buffer: compared.and_then(|compared| compared.open.as_ref().map(|(id, _)| *id)),
            hunks: file,
        });
    }
    files.sort_by(|a, b| a.shown.cmp(&b.shown));
    Ok(files)
}

/// The files changed in the work tree `top` since commit `base`, tracked
/// or not, and those changed in buffers of `known` and not saved: each by
/// its path, and its path from the top, as git names it.
fn changed_paths(
    top: &Path,
    base: &str,
    known: &[Compared],
) -> Result<Vec<(PathBuf, String)>, String> {
    let mut paths = BTreeSet::new();
    let changed = output_within(
        git_filtering(top)?.args([
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            // Nor any differ or converter a repository names.
            "--no-ext-diff",
            "--no-textconv",
            // Nor look inside submodules, which would run `git status` in
            // each, with drivers of their own.
            "--ignore-submodules=dirty",
            base,
            "--",
        ]),
        PATIENCE,
    )
    .ok_or("git couldn't say what's changed since the base")?;
    let untracked = output_within(
        git(top)?.args(["ls-files", "--others", "--exclude-standard", "-z"]),
        PATIENCE,
    )
    .unwrap_or_default();
    for name in changed
        .split(|&byte| byte == 0)
        .chain(untracked.split(|&byte| byte == 0))
    {
        if let Ok(name) = std::str::from_utf8(name)
            && !name.is_empty()
        {
            paths.insert(top.join(name));
        }
    }
    // Files changed in buffers and not saved, which git can't see.
    for compared in known {
        if compared.open.is_some() && compared.path.starts_with(top) {
            paths.insert(compared.path.clone());
        }
    }
    if paths.len() > MOST_FILES {
        return Err(format!(
            "{} files have changed since the base: too many to review here",
            paths.len()
        ));
    }
    // A path with a line break can't be named to git on a line.
    Ok((paths.into_iter())
        .filter_map(|path| {
            let relative = path.strip_prefix(top).ok()?.to_str()?.to_owned();
            (!relative.contains('\n')).then_some((path, relative))
        })
        .collect())
}

/// The commit the branch checked out in the work tree `top` started from:
/// where it meets `main`, `master`, or the branch's upstream.
fn merge_base(top: &Path) -> Result<String, String> {
    for other in ["main", "master", "@{upstream}"] {
        if let Some(found) = output_within(git(top)?.args(["merge-base", "HEAD", other]), PATIENCE)
            && let Ok(found) = String::from_utf8(found)
            && !found.trim().is_empty()
        {
            return Ok(found.trim().to_owned());
        }
    }
    Err("there's no main, master or upstream branch to find the base with".to_owned())
}

/// The text of each of `objects`, blobs in the repository whose work
/// tree's top is `top`, as one `git cat-file --batch` reads them: `None`
/// for one that isn't text.
fn read_blobs(top: &Path, objects: &[&str]) -> Result<HashMap<String, Option<String>>, String> {
    let mut found = HashMap::new();
    if objects.is_empty() {
        return Ok(found);
    }
    let asked: Vec<u8> = objects
        .iter()
        .flat_map(|object| format!("{object}\n").into_bytes())
        .collect();
    let printed = output_fed(git(top)?.args(["cat-file", "--batch"]), asked, PATIENCE)
        .ok_or("git couldn't read the files at the base")?;
    // Each is "OBJECT TYPE SIZE\n", the bytes, and "\n".
    let mut at = 0;
    while at < printed.len() {
        let Some(end) = printed[at..].iter().position(|&byte| byte == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&printed[at..at + end]).into_owned();
        at += end + 1;
        let mut fields = header.split(' ');
        let (Some(object), Some(_), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(size) = size.parse::<usize>() else {
            continue;
        };
        let Some(blob) = printed.get(at..at + size) else {
            break;
        };
        at += size + 1;
        let text = (!blob.contains(&0))
            .then(|| String::from_utf8(blob.to_vec()).ok())
            .flatten()
            .map(buffer::lf);
        found.insert(object.to_owned(), text);
    }
    Ok(found)
}

/// The hunks of the file at `path` against `then`, its text at the base,
/// or `None` if it has none, or can't be compared, as a file that isn't
/// text.
fn compare(then: &str, path: &Path, compared: Option<&Compared>) -> Option<Vec<ReviewHunk>> {
    let (now, agents) = if let Some((_, text)) = compared.and_then(|known| known.open.as_ref()) {
        (text.clone(), compared.map(|known| &known.agents))
    } else {
        let text = match buffer::read(path).ok() {
            Some((text, _)) if text.len() <= MOST_BYTES => buffer::lf(text),
            Some(_) => return None,
            None => String::new(),
        };
        // The agent hunks of a file not open fit it only as it was.
        let stamp = DiskStamp::now(path).ok().flatten();
        let agents = compared.filter(|known| stamp.is_some() && known.stamp == stamp);
        (Rope::from_str(&text), agents.map(|known| &known.agents))
    };
    let now_text = now.to_string();
    let hunks = Hunks::between(then, &now_text);
    if hunks.is_empty() {
        return None;
    }
    let listed = (hunks.all().iter())
        .map(|hunk| {
            let lines = hunk.lines(&now);
            let whole = hunk.whole_lines(&now);
            let (before, after) = context(&now, lines.start, lines.end);
            let overlapping: Vec<&AgentHunk> = (agents.map(AgentHunks::all).unwrap_or_default())
                .iter()
                .filter(|agent| {
                    let (start, end) = (agent.start, agent.end.max(agent.start));
                    if start == end || whole.start == whole.end {
                        start <= whole.end && whole.start <= end
                    } else {
                        start < whole.end && whole.start < end
                    }
                })
                .collect();
            ReviewHunk {
                line: number(lines.start),
                before,
                base: hunk.base.clone(),
                text: now.byte_slice(whole.clone()).to_string(),
                after,
                agents: overlapping.iter().map(|agent| agent.id).collect(),
                by: overlapping.last().map(|agent| Made {
                    chat: TerminalId(agent.chat),
                    turn: agent.turn,
                }),
                reviewed: overlapping.iter().all(|agent| agent.reviewed),
            }
        })
        .collect();
    Some(listed)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use clackatoo_text::{Edit, Maker};

    use super::*;

    /// Runs git in `dir`, as a person would, and checks it worked.
    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn changes_since_the_base_are_found_with_the_agents_marked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        run(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "first"]);
        run(&root, &["checkout", "-q", "-b", "work"]);
        // Committed on the branch, changed on disk, and new.
        std::fs::write(root.join("b.txt"), "B\n").unwrap();
        run(&root, &["commit", "-q", "-am", "second"]);
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        // Open, with an agent's change and the user's, not saved.
        let text = "ONE\ntwo\nTHREE\n";
        let mut agents = AgentHunks::new();
        let before = "one\ntwo\nthree\n";
        let after = "ONE\ntwo\nthree\n";
        let maker = Maker { chat: 4, turn: 1 };
        agents.add(
            &Edit::between(before, after),
            (before, after),
            (maker, &|_| true),
            &mut 7,
        );
        let known = [Compared {
            path: root.join("a.txt"),
            open: Some((BufferId(1), Rope::from_str(text))),
            agents,
            stamp: None,
        }];
        let files = changes_since_base(&root, &known).unwrap();
        let shown: Vec<&str> = files.iter().map(|file| file.shown.as_str()).collect();
        assert_eq!(shown, ["a.txt", "b.txt", "new.txt"]);
        let a = &files[0];
        assert_eq!(a.buffer, Some(BufferId(1)));
        let marks: Vec<(&str, Vec<u64>, Option<Made>)> = (a.hunks.iter())
            .map(|hunk| (hunk.text.as_str(), hunk.agents.clone(), hunk.by))
            .collect();
        let by = Made {
            chat: TerminalId(4),
            turn: 1,
        };
        assert_eq!(
            marks,
            [("ONE\n", vec![7], Some(by)), ("THREE\n", vec![], None)]
        );
        assert_eq!(files[1].hunks[0].base, "b\n");
        assert_eq!(files[2].hunks[0].text, "new\n");
    }

    #[test]
    fn a_submodules_own_filter_drivers_never_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        run(&sub, &["init", "-q", "-b", "main"]);
        std::fs::write(sub.join(".gitattributes"), "a.txt filter=evil\n").unwrap();
        std::fs::write(sub.join("a.txt"), "was\n").unwrap();
        run(&sub, &["add", "."]);
        run(&sub, &["commit", "-q", "-m", "first"]);
        run(&root, &["init", "-q", "-b", "main"]);
        run(&root, &["add", "sub"]);
        run(&root, &["commit", "-q", "-m", "first"]);
        // The submodule's config names a driver the work tree's doesn't,
        // and its file has changed, but not in size, so `git status` in
        // it would have to clean the file to compare it.
        let outside = tempfile::tempdir().unwrap();
        let ran = outside.path().join("ran");
        let driver = format!("touch '{}'; cat", ran.display());
        run(&sub, &["config", "filter.evil.clean", &driver]);
        std::fs::write(sub.join("a.txt"), "new\n").unwrap();
        assert_eq!(changes_since_base(&root, &[]), Ok(Vec::new()));
        // Nor as an explorer asks.
        assert_eq!(crate::git::status(&root), Some(Vec::new()));
        assert!(!ran.exists(), "the submodule's driver ran");
        // It would have run, looking inside the submodule.
        run(&root, &["diff", "--name-only", "HEAD", "--"]);
        assert!(ran.exists());
    }

    #[test]
    fn a_tree_without_a_base_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(changes_since_base(&root, &[]).is_err());
        run(&root, &["init", "-q", "-b", "trunk"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "first"]);
        assert_eq!(
            changes_since_base(&root, &[]),
            Err("there's no main, master or upstream branch to find the base with".into())
        );
    }

    #[test]
    fn context_is_three_lines_either_side_without_line_ends() {
        let text = Rope::from_str("1\n2\n3\n4\n5\n6\n7\n8\n");
        let (before, after) = context(&text, 4, 5);
        assert_eq!(before, ["2", "3", "4"]);
        assert_eq!(after, ["6", "7", "8"]);
        // None past either end, and none for the line after the last end.
        let (before, after) = context(&text, 0, 7);
        assert!(before.is_empty());
        assert_eq!(after, ["8"]);
        let (_, after) = context(&text, 0, 8);
        assert!(after.is_empty());
    }
}
