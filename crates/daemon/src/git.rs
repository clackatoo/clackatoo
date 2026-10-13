//! The thread that compares buffers with git: the event loop hands it each
//! buffer's text as it changes, and it hands back the buffer's hunks, the
//! lines added, changed and deleted since the file's text in git's index,
//! so neither git nor a comparison ever holds up a keystroke.
//!
//! The base is the index, as gitsigns' is, not `HEAD`: the signs show
//! what isn't staged yet, so staging a hunk takes its sign away. A file
//! git has conflicts in has no text in the index of its own, so it's
//! compared with ours, the side `HEAD` has, and can't be staged by hunks.
//! A file that isn't in the index, being new or ignored, has no hunks.
//!
//! git is run as a program, as the editor's other tools are, rather than
//! read with a library: it's there wherever a repository is, it reads
//! every kind of repository and setting as only git does, and it costs
//! the daemon nothing to build or start. It runs only when a buffer is
//! opened, when the index changes, and to stage: a comparison after an
//! edit needs no git at all, only the base it kept.
//!
//! Comparisons wait until edits pause for [`PAUSE`], as gitsigns' do, so
//! a burst of typing is compared once, but no longer than [`MOST_WAIT`]
//! after the first edit not compared, so typing on and on still shows its
//! signs now and then. Opening a buffer, the index changing and staging
//! are acted on at once.
//!
//! A file git converts on its way in and out of the index, by a filter
//! such as Git LFS's or by line ends, is compared with its text as git
//! would check it out, so only what's really changed is signed. Hunks of
//! it aren't staged: `git apply --cached` would put the work tree's bytes
//! in the index unconverted.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, GitMark};
use clackatoo_text::{Difference, Hunks, Rope, diff, line_starts};

/// How long edits must pause before a buffer is compared again...
pub(crate) const PAUSE: Duration = Duration::from_millis(100);

/// ...but no longer than this after the first edit since it last was.
const MOST_WAIT: Duration = Duration::from_secs(1);

/// Texts bigger than this aren't compared: they're data, or built, not
/// written, as the finder doesn't grep them.
const MAX_TEXT: usize = 8 * 1024 * 1024;

/// How many lines either side of a hunk a patch to stage it shows, as
/// `git diff` does, so git checks the index still has them there.
const CONTEXT: usize = 3;

/// A buffer, as the event loop numbers it.
pub(crate) type Key = u64;

/// What the thread is asked to do.
#[derive(Debug)]
enum Job {
    Open {
        buffer: Key,
        path: PathBuf,
        text: Rope,
        version: u64,
    },
    Edit {
        buffer: Key,
        text: Rope,
        version: u64,
    },
    Close(Key),
    /// The index of the repository whose git directory this is changed.
    IndexChanged(PathBuf),
    Stage {
        buffer: Key,
        text: Rope,
        version: u64,
        at: usize,
        ticket: u64,
    },
}

/// What the thread hands back.
#[derive(Debug)]
pub(crate) enum Found {
    /// A buffer's file is in the work tree of the repository whose git
    /// directory this is, whose index the loop is to watch.
    Repository { buffer: Key, git_dir: PathBuf },
    /// A buffer's hunks, against its text at `version`.
    Hunks {
        buffer: Key,
        version: u64,
        hunks: Hunks,
    },
    /// How the stage the loop numbered `ticket` went: staged, nothing to
    /// stage there, or why git refused.
    Staged { ticket: u64, answer: Answer },
}

/// The thread, and what it's found.
#[derive(Debug)]
pub(crate) struct Git {
    jobs: Sender<Job>,
    found: Receiver<Found>,
}

impl Git {
    /// Starts the thread. It calls `wake` when it has something to take
    /// with [`take`](Self::take), and stops when this is dropped.
    pub(crate) fn start(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (jobs, taken) = mpsc::channel();
        let (send, found) = mpsc::channel();
        thread::Builder::new()
            .name("git".to_owned())
            .spawn(move || Worker::new(send, Box::new(wake)).run(&taken))?;
        Ok(Self { jobs, found })
    }

    /// Starts comparing `buffer`, the file at `path`, whose text is `text`
    /// at `version`, if it's in a git work tree.
    pub(crate) fn open(&self, buffer: Key, path: PathBuf, text: Rope, version: u64) {
        self.send(Job::Open {
            buffer,
            path,
            text,
            version,
        });
    }

    /// Tells it `buffer`'s text is now `text`, at `version`.
    pub(crate) fn edit(&self, buffer: Key, text: Rope, version: u64) {
        self.send(Job::Edit {
            buffer,
            text,
            version,
        });
    }

    /// Forgets `buffer`.
    pub(crate) fn close(&self, buffer: Key) {
        self.send(Job::Close(buffer));
    }

    /// Tells it the index of the repository whose git directory is
    /// `git_dir` has changed.
    pub(crate) fn index_changed(&self, git_dir: PathBuf) {
        self.send(Job::IndexChanged(git_dir));
    }

    /// Stages the hunk at byte `at` of `buffer`'s text, which is `text`
    /// at `version`, answering with `ticket`.
    pub(crate) fn stage(&self, buffer: Key, (text, version): (Rope, u64), at: usize, ticket: u64) {
        self.send(Job::Stage {
            buffer,
            text,
            version,
            at,
            ticket,
        });
    }

    /// What it's found since last asked, in the order it found them.
    pub(crate) fn take(&self) -> impl Iterator<Item = Found> + '_ {
        self.found.try_iter()
    }

    fn send(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            log::warn!("the git thread has stopped");
        }
    }
}

/// A buffer whose file is in a git work tree.
#[derive(Debug)]
struct Tracked {
    /// The work tree's top directory, and the repository's git directory.
    top: PathBuf,
    git_dir: PathBuf,
    /// The file's path from `top`, with `/` between its parts.
    path: String,
    /// The file's text in the index, if it's there.
    base: Option<Base>,
    /// git converts the file on its way in and out of the index.
    converted: bool,
    text: Rope,
    version: u64,
    /// It's been edited since it was last compared.
    edited: bool,
}

/// What a buffer is compared with.
#[derive(Debug)]
struct Base {
    /// The index entry it's from, which says whether it's changed.
    entry: Entry,
    /// Its text, or `None` if it isn't text, or is too big to compare.
    text: Option<String>,
    /// Its lines end with CRLF in the index, which a patch made from the
    /// buffer, whose lines end `\n`, wouldn't match.
    crlf: bool,
}

/// A file's entry in the index.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    /// The object its text is.
    object: String,
    /// Whether hunks can be staged on it: not when it's ours of a
    /// conflict.
    stageable: bool,
}

struct Worker {
    /// Each buffer, or `None` for one whose file isn't in a work tree.
    buffers: HashMap<Key, Option<Tracked>>,
    found: Sender<Found>,
    wake: Box<dyn Fn() + Send>,
    /// Something was sent since the loop was last woken.
    unwoken: bool,
}

impl Worker {
    fn new(found: Sender<Found>, wake: Box<dyn Fn() + Send>) -> Self {
        Self {
            buffers: HashMap::new(),
            found,
            wake,
            unwoken: false,
        }
    }

    fn run(mut self, jobs: &Receiver<Job>) {
        // When the buffers edited since they were compared are compared,
        // and when the first of those edits came.
        let mut due: Option<Instant> = None;
        let mut first: Option<Instant> = None;
        loop {
            let job = match due {
                None => match jobs.recv() {
                    Ok(job) => Some(job),
                    Err(_) => return,
                },
                Some(due) => match jobs.recv_timeout(due.saturating_duration_since(Instant::now()))
                {
                    Ok(job) => Some(job),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            match job {
                Some(Job::Edit {
                    buffer,
                    text,
                    version,
                }) => {
                    if let Some(Some(tracked)) = self.buffers.get_mut(&buffer) {
                        tracked.text = text;
                        tracked.version = version;
                        tracked.edited = true;
                        let now = Instant::now();
                        let first = *first.get_or_insert(now);
                        due = Some((now + PAUSE).min(first + MOST_WAIT));
                    }
                }
                Some(job) => self.take(job),
                None => {
                    (due, first) = (None, None);
                    let edited: Vec<Key> = (self.buffers.iter())
                        .filter(|(_, tracked)| tracked.as_ref().is_some_and(|t| t.edited))
                        .map(|(&buffer, _)| buffer)
                        .collect();
                    for buffer in edited {
                        self.compare(buffer);
                    }
                }
            }
            if std::mem::take(&mut self.unwoken) {
                (self.wake)();
            }
        }
    }

    /// Does `job`, which isn't an edit.
    fn take(&mut self, job: Job) {
        match job {
            Job::Open {
                buffer,
                path,
                text,
                version,
            } => {
                let tracked = repository(&path).map(|(top, git_dir, path)| Tracked {
                    top,
                    git_dir,
                    path,
                    base: None,
                    converted: false,
                    text,
                    version,
                    edited: false,
                });
                let Some(mut tracked) = tracked else {
                    self.buffers.insert(buffer, None);
                    return;
                };
                let git_dir = tracked.git_dir.clone();
                tracked.converted = converted(&tracked.top, &tracked.path);
                tracked.base = base_of(&tracked.top, &tracked.path, tracked.converted);
                self.buffers.insert(buffer, Some(tracked));
                self.send(Found::Repository { buffer, git_dir });
                self.compare(buffer);
            }
            Job::Close(buffer) => {
                self.buffers.remove(&buffer);
            }
            Job::IndexChanged(git_dir) => self.reread(&git_dir),
            Job::Stage {
                buffer,
                text,
                version,
                at,
                ticket,
            } => {
                let answer = self.stage(buffer, text, version, at);
                self.send(Found::Staged { ticket, answer });
            }
            Job::Edit { .. } => unreachable!("edits wait"),
        }
    }

    /// Reads the bases of the buffers in the repository whose git
    /// directory is `git_dir` again, comparing those whose base changed.
    /// Only the bases whose index entries changed are read.
    fn reread(&mut self, git_dir: &Path) {
        let mut tops: HashMap<PathBuf, Vec<(Key, String)>> = HashMap::new();
        for (&buffer, tracked) in &self.buffers {
            if let Some(tracked) = tracked
                && tracked.git_dir == git_dir
            {
                let paths = tops.entry(tracked.top.clone()).or_default();
                paths.push((buffer, tracked.path.clone()));
            }
        }
        for (top, buffers) in tops {
            let paths: Vec<&str> = buffers.iter().map(|(_, path)| path.as_str()).collect();
            let mut entries = list_index(&top, &paths);
            for (buffer, path) in &buffers {
                let entry = entries.remove(path);
                let Some(Some(tracked)) = self.buffers.get_mut(buffer) else {
                    continue;
                };
                let same = match (&tracked.base, &entry) {
                    (Some(base), Some(entry)) => base.entry == *entry,
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    let converted = tracked.converted;
                    tracked.base = entry.map(|entry| read_base(&top, path, entry, converted));
                    self.compare(*buffer);
                }
            }
        }
    }

    /// Compares `buffer` with its base, and sends its hunks.
    fn compare(&mut self, buffer: Key) {
        let Some(Some(tracked)) = self.buffers.get_mut(&buffer) else {
            return;
        };
        tracked.edited = false;
        let hunks = match base_text(tracked) {
            Some(base) => Hunks::between(base, &tracked.text.to_string()),
            None => Hunks::new(),
        };
        let version = tracked.version;
        self.send(Found::Hunks {
            buffer,
            version,
            hunks,
        });
    }

    /// Stages the hunk at byte `at` of `buffer`'s text, which is `text`
    /// at `version`, then reads its base again and compares it.
    fn stage(&mut self, buffer: Key, text: Rope, version: u64, at: usize) -> Answer {
        let Some(Some(tracked)) = self.buffers.get_mut(&buffer) else {
            return Answer::Failed("this file isn't in a git repository".into());
        };
        tracked.text = text;
        tracked.version = version;
        match &tracked.base {
            None => return Answer::Failed("this file isn't in git's index".into()),
            Some(base) if !base.entry.stageable => {
                return Answer::Failed("this file has conflicts: resolve them to stage it".into());
            }
            Some(_) if tracked.converted => {
                return Answer::Failed("git converts this file: stage it in a terminal".into());
            }
            Some(base) if base.crlf => {
                return Answer::Failed(
                    "this file's lines end with CRLF: stage it in a terminal".into(),
                );
            }
            Some(_) => {}
        }
        // The index may have changed since its base was read, and the
        // watch not been heard yet: staging on a stale base could undo
        // what was staged meanwhile.
        let entry = list_index(&tracked.top, &[&tracked.path]).remove(&tracked.path);
        if tracked.base.as_ref().map(|base| &base.entry) != entry.as_ref() {
            let (top, file) = (tracked.top.clone(), tracked.path.clone());
            tracked.base = entry.map(|entry| read_base(&top, &file, entry, false));
            self.compare(buffer);
            return Answer::Failed("git's index changed just now: try again".into());
        }
        let Some(base) = base_text(tracked) else {
            return Answer::Failed("this file is too big to compare, or isn't text".into());
        };
        let text = tracked.text.to_string();
        let line = tracked.text.byte_to_line(at.min(text.len()));
        let differences = diff(base, &text);
        let hunks = Hunks::of_differences(base, &text, &differences);
        let Some((index, _)) = hunks.find(&tracked.text, line) else {
            return Answer::Nothing;
        };
        let patch = patch(&tracked.path, base, &text, &differences[index]);
        if let Err(why) = apply(&tracked.top, &patch) {
            return Answer::Failed(format!("git couldn't stage it: {why}"));
        }
        let (top, file) = (tracked.top.clone(), tracked.path.clone());
        tracked.base = base_of(&top, &file, false);
        self.compare(buffer);
        Answer::Staged
    }

    fn send(&mut self, found: Found) {
        if self.found.send(found).is_ok() {
            self.unwoken = true;
        }
    }
}

/// The text `tracked` is compared with, if it has one and neither it nor
/// the buffer's text is too big.
fn base_text(tracked: &Tracked) -> Option<&str> {
    let base = tracked.base.as_ref()?.text.as_deref()?;
    (base.len() <= MAX_TEXT && tracked.text.len_bytes() <= MAX_TEXT).then_some(base)
}

/// git, to run in `dir`, reading nothing, seeing no repository but the
/// one `dir` is in, and taking none of the locks it can do without, so
/// reading never writes the index it's watched for.
fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Paths are paths, never patterns.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null());
    command
}

/// What git printed, if it ran and succeeded.
fn output(command: &mut Command) -> Option<Vec<u8>> {
    match command.output() {
        Ok(output) if output.status.success() => Some(output.stdout),
        Ok(_) => None,
        Err(err) => {
            log::debug!("git didn't run: {err}");
            None
        }
    }
}

/// What git printed, if it ran and succeeded within `patience`: one that
/// takes longer, as on a hung network file system, is killed, so the
/// thread asking goes on to what's waiting behind it.
pub(crate) fn output_within(command: &mut Command, patience: Duration) -> Option<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| log::debug!("git didn't run: {err}"))
        .ok()?;
    // Read as it's written, so a lot of it never fills the pipe and holds
    // git up.
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut read).map(|_| read)
    });
    let deadline = Instant::now() + patience;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                log::warn!("git took longer than {patience:?}: stopped");
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Err(err) => {
                log::debug!("waiting for git failed: {err}");
                break None;
            }
        }
    };
    let read = reader.join().ok()?.ok()?;
    status
        .filter(std::process::ExitStatus::success)
        .map(|_| read)
}

/// How long git is given to say what explorers ask it: far longer than it
/// takes in any repository on a working disk.
const PATIENCE: Duration = Duration::from_secs(10);

/// The work tree the file at `path` is in, if any: its top directory, its
/// repository's git directory, and the file's path from the top.
fn repository(path: &Path) -> Option<(PathBuf, PathBuf, String)> {
    let (top, git_dir) = work_tree(path.parent()?)?;
    let relative = (path.strip_prefix(&top).ok().map(Path::to_owned))
        .or_else(|| Some(path.strip_prefix(top.canonicalize().ok()?).ok()?.to_owned()))?;
    let relative = relative.to_str()?.to_owned();
    // A path with a line break can't be named to git on a line.
    (!relative.is_empty() && !relative.contains('\n')).then_some((top, git_dir, relative))
}

/// The work tree the directory `dir` is in, if any: its top directory,
/// and its repository's git directory.
pub(crate) fn work_tree(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let printed = output_within(
        git(dir).args(["rev-parse", "--show-toplevel", "--absolute-git-dir"]),
        PATIENCE,
    )?;
    let printed = String::from_utf8(printed).ok()?;
    let mut lines = printed.lines();
    Some((PathBuf::from(lines.next()?), PathBuf::from(lines.next()?)))
}

/// What git says of the files in the work tree whose top is `top`: each
/// path it has something to say about, from the top, with its mark, in
/// the order git gives them. A directory none of whose files git knows is
/// one path, as `git status` shows it. `None` if git couldn't say.
///
/// Changes in submodules' own files aren't looked for, which would read
/// every submodule's whole tree.
pub(crate) fn status(top: &Path) -> Option<Vec<(String, GitMark)>> {
    let printed = output_within(
        git(top).args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--no-renames",
            "--ignore-submodules=dirty",
        ]),
        PATIENCE,
    )?;
    // Each is "XY PATH": X for the index, Y for the work tree.
    let marks = (printed.split(|&byte| byte == 0))
        .filter_map(|entry| {
            let (xy, path) = (entry.get(..2)?, entry.get(3..)?);
            let path = std::str::from_utf8(path).ok()?.trim_end_matches('/');
            (!path.is_empty()).then(|| (path.to_owned(), mark(xy[0], xy[1])))
        })
        .collect();
    Some(marks)
}

/// The mark for a file `git status` says `XY` of: what its index has
/// against `HEAD`, then what its work tree has against the index.
fn mark(x: u8, y: u8) -> GitMark {
    match (x, y) {
        (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => GitMark::Conflicted,
        (b'?', _) => GitMark::Untracked,
        (b'D', _) | (_, b'D') => GitMark::Deleted,
        (b'A', b' ') => GitMark::Added,
        _ => GitMark::Modified,
    }
}

/// The base of the file at `path` from `top`, as git's index has it, if
/// it's there, as git would check it out if it's `converted`.
fn base_of(top: &Path, path: &str, converted: bool) -> Option<Base> {
    let entry = list_index(top, &[path]).remove(path)?;
    Some(read_base(top, path, entry, converted))
}

/// The text of the index entry `entry`, of the file at `path` in the work
/// tree whose top is `top`: as it's kept, or as git would check it out if
/// it's `converted`.
fn read_base(top: &Path, path: &str, entry: Entry, converted: bool) -> Base {
    let mut command = git(top);
    if converted {
        command.args([
            "cat-file",
            "--filters",
            &format!("--path={path}"),
            &entry.object,
        ]);
    } else {
        command.args(["cat-file", "blob", &entry.object]);
    }
    let text = output(&mut command)
        .filter(|blob| blob.len() <= MAX_TEXT && !blob.contains(&0))
        .and_then(|blob| String::from_utf8(blob).ok());
    // Kept with CRLF, it's compared as the buffer keeps it, with `\n`.
    let crlf = text.as_ref().is_some_and(|text| text.contains("\r\n"));
    let text = text.map(crate::buffer::lf);
    Base { entry, text, crlf }
}

/// Whether git converts the file at `path` from `top` on its way in and
/// out of the index: it has a filter, such as Git LFS's, or its line ends
/// are converted, by its attributes or by `core.autocrlf`.
fn converted(top: &Path, path: &str) -> bool {
    let Some(said) =
        output(git(top).args(["check-attr", "-z", "filter", "text", "eol", "--", path]))
    else {
        return false;
    };
    // "PATH\0ATTRIBUTE\0VALUE\0" for each.
    let said: Vec<&[u8]> = said.split(|&byte| byte == 0).collect();
    let value = |name: &[u8]| {
        (said.chunks(3))
            .find(|fields| fields.get(1) == Some(&name))
            .and_then(|fields| fields.get(2).copied())
            .unwrap_or(b"unspecified")
    };
    let specified = |value: &[u8]| value != b"unspecified" && value != b"unset";
    if specified(value(b"filter")) || specified(value(b"eol")) {
        return true;
    }
    let text = value(b"text");
    if text == b"unset" {
        return false;
    }
    let setting = |name: &str| {
        let said = output(git(top).args(["config", "--get", name])).unwrap_or_default();
        String::from_utf8_lossy(&said).trim().to_ascii_lowercase()
    };
    let autocrlf = setting("core.autocrlf");
    autocrlf == "true" || autocrlf == "input" || (specified(text) && setting("core.eol") == "crlf")
}

/// The index entries of the files at `paths` from `top`, by path. A file
/// not in the index has none.
fn list_index(top: &Path, paths: &[&str]) -> HashMap<String, Entry> {
    let mut found = HashMap::new();
    let Some(listed) = output(
        git(top)
            .args(["ls-files", "--stage", "-z", "--"])
            .args(paths),
    ) else {
        return found;
    };
    // Each entry is "MODE OBJECT STAGE\tPATH": stage 0 for a file without
    // conflicts, or 1 to 3 for the base, ours and theirs of one with.
    let mut entries: HashMap<&[u8], [Option<&str>; 4]> = HashMap::new();
    for entry in listed.split(|&byte| byte == 0) {
        let Some(tab) = entry.iter().position(|&byte| byte == b'\t') else {
            continue;
        };
        let (fields, path) = (&entry[..tab], &entry[tab + 1..]);
        let Ok(fields) = std::str::from_utf8(fields) else {
            continue;
        };
        let mut fields = fields.split(' ');
        let (Some(mode), Some(object), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        // Only files: not links, whose text is where they point, nor
        // submodules.
        let Ok(stage) = stage.parse::<usize>() else {
            continue;
        };
        if !mode.starts_with("100") || stage > 3 {
            continue;
        }
        entries.entry(path).or_default()[stage] = Some(object);
    }
    for path in paths {
        let Some(stages) = entries.get(path.as_bytes()) else {
            continue;
        };
        let (object, stageable) = match stages {
            [Some(object), ..] => (*object, true),
            [None, _, Some(ours), _] => (*ours, false),
            _ => continue,
        };
        let object = object.to_owned();
        found.insert((*path).to_owned(), Entry { object, stageable });
    }
    found
}

/// A patch that makes `base`, the text in the index of the file at `path`
/// from the work tree's top, as `text` is for the lines `difference`
/// says, and nowhere else: what `git apply --cached` takes to stage that
/// hunk alone. It has a few lines either side the same in both, as
/// `git diff` shows, so git checks they're still there.
fn patch(file: &str, base: &str, text: &str, difference: &Difference) -> String {
    let (base_lines, text_lines) = (line_starts(base), line_starts(text));
    let base_line = |line: usize| &base[base_lines[line]..base_lines[line + 1]];
    let text_line = |line: usize| &text[text_lines[line]..text_lines[line + 1]];
    let Difference { before, after } = difference;
    let first = before.start.saturating_sub(CONTEXT);
    let last = (before.end + CONTEXT).min(base_lines.len() - 1);
    let same = (first..before.start).len() + (before.end..last).len();
    let (old, new) = (same + before.len(), same + after.len());
    // A side with no lines is numbered by the line before it.
    let start = |count: usize| if count == 0 { first } else { first + 1 };
    // git quotes a name whole, its a/ or b/ with it.
    let (a, b) = (quoted(&format!("a/{file}")), quoted(&format!("b/{file}")));
    let mut patch = format!(
        "diff --git {a} {b}\n--- {a}\n+++ {b}\n@@ -{},{old} +{},{new} @@\n",
        start(old),
        start(new),
    );
    let mut push = |mark: char, line: &str| {
        patch.push(mark);
        patch.push_str(line);
        if !line.ends_with('\n') {
            patch.push_str("\n\\ No newline at end of file\n");
        }
    };
    for line in first..before.start {
        push(' ', base_line(line));
    }
    for line in before.clone() {
        push('-', base_line(line));
    }
    for line in after.clone() {
        push('+', text_line(line));
    }
    for line in before.end..last {
        push(' ', base_line(line));
    }
    patch
}

/// `path` as a patch names it: as it is, or in quotes, with C's escapes,
/// if it has a quote, a backslash or a control character in it, as git
/// writes such paths.
fn quoted(path: &str) -> String {
    if !path
        .chars()
        .any(|ch| ch == '"' || ch == '\\' || ch.is_control())
    {
        return path.to_owned();
    }
    let mut quoted = String::from("\"");
    for ch in path.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\t' => quoted.push_str("\\t"),
            '\n' => quoted.push_str("\\n"),
            ch if ch.is_control() => {
                let mut bytes = [0; 4];
                for byte in ch.encode_utf8(&mut bytes).bytes() {
                    let _ = write!(quoted, "\\{byte:03o}");
                }
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// Applies `patch` to the index of the work tree whose top is `top`, or
/// says why git wouldn't.
fn apply(top: &Path, patch: &str) -> Result<(), String> {
    let mut child = git(top)
        .args(["apply", "--cached", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("git didn't run: {err}"))?;
    // A patch is a hunk's lines, small enough to write before reading
    // what git says; git reads all of it before it says anything.
    if let Some(mut stdin) = child.stdin.take()
        && let Err(err) = stdin.write_all(patch.as_bytes())
    {
        log::debug!("writing a patch to git failed: {err}");
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("git didn't finish: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let first = said.lines().find(|line| !line.trim().is_empty());
    Err(first.unwrap_or("it gave no reason").trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_for(base: &str, text: &str, index: usize) -> String {
        let differences = diff(base, text);
        let patch = patch("src/a.rs", base, text, &differences[index]);
        // Only the hunk, under its header.
        patch.lines().skip(3).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_patch_stages_one_hunk_with_lines_either_side() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let text = "1\n2\nthree\n4\n5\n6\n7\n8\n9\nten\n";
        assert_eq!(
            patch_for(base, text, 0),
            "@@ -1,6 +1,6 @@\n 1\n 2\n-3\n+three\n 4\n 5\n 6"
        );
        assert_eq!(
            patch_for(base, text, 1),
            "@@ -7,4 +7,4 @@\n 7\n 8\n 9\n-10\n+ten"
        );
    }

    #[test]
    fn a_patch_numbers_a_side_with_no_lines_by_the_line_before() {
        // Into an empty file, and everything taken out of one.
        assert_eq!(patch_for("", "a\nb\n", 0), "@@ -0,0 +1,2 @@\n+a\n+b");
        assert_eq!(patch_for("a\n", "", 0), "@@ -1,1 +0,0 @@\n-a");
    }

    #[test]
    fn a_patch_says_where_a_line_has_no_end() {
        assert_eq!(
            patch_for("a\nb", "a\nc", 0),
            "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file"
        );
        assert_eq!(
            patch_for("a\nb\n", "a\nb", 0),
            "@@ -1,2 +1,2 @@\n a\n-b\n+b\n\\ No newline at end of file"
        );
    }

    #[test]
    fn a_patch_quotes_a_name_whole_with_its_side() {
        let differences = diff("a\n", "b\n");
        let patch = patch("x\ty", "a\n", "b\n", &differences[0]);
        let header: Vec<&str> = patch.lines().take(3).collect();
        assert_eq!(
            header,
            [
                "diff --git \"a/x\\ty\" \"b/x\\ty\"",
                "--- \"a/x\\ty\"",
                "+++ \"b/x\\ty\""
            ]
        );
    }

    #[test]
    fn a_command_that_takes_too_long_is_stopped() {
        let started = Instant::now();
        let mut slow = Command::new("sleep");
        slow.arg("5");
        assert_eq!(output_within(&mut slow, Duration::from_millis(100)), None);
        assert!(started.elapsed() < Duration::from_secs(2));
        let mut quick = Command::new("echo");
        quick.arg("hi");
        let said = output_within(&mut quick, Duration::from_secs(5));
        assert_eq!(said.as_deref(), Some(&b"hi\n"[..]));
    }

    #[test]
    fn marks_say_what_matters_most_of_both_sides() {
        assert_eq!(mark(b'?', b'?'), GitMark::Untracked);
        assert_eq!(mark(b'A', b' '), GitMark::Added);
        assert_eq!(mark(b'A', b'M'), GitMark::Modified);
        assert_eq!(mark(b' ', b'M'), GitMark::Modified);
        assert_eq!(mark(b'M', b'D'), GitMark::Deleted);
        assert_eq!(mark(b'U', b'U'), GitMark::Conflicted);
        assert_eq!(mark(b'A', b'A'), GitMark::Conflicted);
    }

    #[test]
    fn odd_paths_are_quoted_as_git_quotes_them() {
        assert_eq!(quoted("src/a b.rs"), "src/a b.rs");
        assert_eq!(quoted("a\"b\\c\td"), "\"a\\\"b\\\\c\\td\"");
        assert_eq!(quoted("bell\u{7}"), "\"bell\\007\"");
    }
}
