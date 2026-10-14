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
//! or by line ends, is compared with its text as git would check it out,
//! so only what's really changed is signed. Hunks of it aren't staged:
//! `git apply --cached` would put the work tree's bytes in the index
//! unconverted. git never runs the repository's own filter drivers here
//! (see [`git_filtering`]), and unless every driver is allowed, Git
//! LFS's files have no signs: checking one out could fetch it.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, GitMark};
use clackatoo_text::{Difference, Hunks, Rope, diff, line_starts};

mod filters;
mod installed;

/// How long edits must pause before a buffer is compared again...
pub(crate) const PAUSE: Duration = Duration::from_millis(100);

/// ...but no longer than this after the first edit since it last was.
const MOST_WAIT: Duration = Duration::from_secs(1);

/// Texts bigger than this aren't compared: they're data, or built, not
/// written, as the finder doesn't grep them.
const MAX_TEXT: usize = 8 * 1024 * 1024;

/// How a Git LFS pointer starts: the text git keeps for a file Git LFS
/// keeps elsewhere, which is no base for the file's own text.
const LFS_POINTER: &str = "version https://git-lfs.github.com/spec/";

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
/// reading never writes the index it's watched for. Or why git can't be
/// run at all (see [`installed`]).
///
/// Nothing a repository's own config names is run: a folder copied from
/// anyone, or a clone with a git directory committed inside it, must be
/// safe to open. A command that can run files through filter drivers,
/// comparing the work tree or checking a file out, is made with
/// [`git_filtering`], which turns those off too.
pub(crate) fn git(dir: &Path) -> Result<Command, &'static str> {
    let installed = installed::installed()?;
    let mut command = Command::new(&installed.program);
    command
        // A command for git to run as it looks at the work tree.
        .args(["-c", "core.fsmonitor=false"])
        // Hooks, which staging a hunk would run, as the index changes: no
        // path is a directory under /dev/null.
        .args(["-c", "core.hooksPath=/dev/null"])
        // A git directory found by itself, not as a work tree's `.git`,
        // as one committed inside a clone would be, with a config of its
        // own: it's a repository only when named, as clackatoo never
        // does. Only the user's own config can say otherwise.
        .args(["-c", "safe.bareRepository=explicit"])
        .current_dir(dir)
        // Neither git nor the shell it runs anything in finds a program
        // in the work tree (see [`installed`]).
        .env("PATH", &installed.path)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Paths are paths, never patterns.
        .env("GIT_LITERAL_PATHSPECS", "1")
        // An object missing from a partial clone is fetched from the
        // remote the repository names, by the commands it names to reach
        // it or log in with: never fetch, by any way at all. git before
        // 2.44 knows only the second, which is enough alone.
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALLOW_PROTOCOL", "")
        // Nothing from where the daemon was started points git at another
        // repository, or at other config in place of the repository's.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG")
        .stdin(Stdio::null())
        // In a process group of its own, so one killed for taking too
        // long takes what it started with it (see [`crate::group`]).
        .process_group(0);
    Ok(command)
}

/// git, as [`git`] has it, to run in the work tree whose top is `top` a
/// command that can run files through filter drivers: with every driver
/// the repository's own config defines turned off, unless it's Git LFS's
/// or `$CLACKATOO_GITFILTERS` allows it (see [`filters`]). Or why it
/// can't be made safe: git couldn't say what drivers there are.
///
/// The config is read, then the command is run: a driver added to the
/// repository's config in between, or to a file it includes, isn't
/// turned off. Only what can already write to the repository, or to what
/// it includes, can do that, and the next command turns it off.
pub(crate) fn git_filtering(top: &Path) -> Result<Command, &'static str> {
    let allowed = filters::Allowed::chosen();
    let mut command = git(top)?;
    if allowed != filters::Allowed::All {
        let config = output_within(
            git(top)?.args(["config", "--list", "-z", "--show-scope"]),
            PATIENCE,
        )
        .ok_or("git couldn't say what filter drivers there are")?;
        filters::set(&mut command, &filters::turned_off(&config, allowed));
    }
    Ok(command)
}

/// Kills `child`, a git not yet reaped, with what it started, and reaps
/// it. Not yet reaped, its process ID is its group's, and no other's.
fn kill(child: &mut std::process::Child) {
    let group = rustix::process::Pid::from_child(child);
    if rustix::process::kill_process_group(group, rustix::process::Signal::KILL).is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
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
                kill(&mut child);
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

/// What git printed, given `input` on its standard input, if it ran and
/// succeeded within `patience`, as [`output_within`] has it.
pub(crate) fn output_fed(
    command: &mut Command,
    input: Vec<u8>,
    patience: Duration,
) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| log::debug!("git didn't run: {err}"))
        .ok()?;
    // Written and read at once, so neither pipe fills and holds git up.
    let mut stdin = child.stdin.take()?;
    let writer = thread::spawn(move || std::io::Write::write_all(&mut stdin, &input));
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
                kill(&mut child);
                break None;
            }
            Err(err) => {
                log::debug!("waiting for git failed: {err}");
                break None;
            }
        }
    };
    let _ = writer.join();
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
        git(dir)
            .ok()?
            .args(["rev-parse", "--show-toplevel", "--absolute-git-dir"]),
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
        git_filtering(top).ok()?.args([
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
    let command = if converted {
        git_filtering(top).ok().map(|mut command| {
            command.args([
                "cat-file",
                "--filters",
                &format!("--path={path}"),
                &entry.object,
            ]);
            // Git LFS fetches what it doesn't have, and fetching runs
            // what the repository's config says to fetch or log in with:
            // reading a base must never do that. So it's told to check
            // nothing out: its files read as their pointers, and have no
            // base.
            if filters::Allowed::chosen() != filters::Allowed::All {
                command.env("GIT_LFS_SKIP_SMUDGE", "1");
            }
            command
        })
    } else {
        git(top).ok().map(|mut command| {
            command.args(["cat-file", "blob", &entry.object]);
            command
        })
    };
    let text = command
        .and_then(|mut command| output(&mut command))
        .filter(|blob| blob.len() <= MAX_TEXT && !blob.contains(&0))
        .and_then(|blob| String::from_utf8(blob).ok())
        .filter(|text| !(converted && text.starts_with(LFS_POINTER)));
    // Kept with CRLF, it's compared as the buffer keeps it, with `\n`.
    let crlf = text.as_ref().is_some_and(|text| text.contains("\r\n"));
    let text = text.map(crate::buffer::lf);
    Base { entry, text, crlf }
}

/// Whether git converts the file at `path` from `top` on its way in and
/// out of the index: it has a filter, such as Git LFS's, or its line ends
/// are converted, by its attributes or by `core.autocrlf`.
fn converted(top: &Path, path: &str) -> bool {
    let Some(said) = git(top).ok().and_then(|mut command| {
        output(command.args(["check-attr", "-z", "filter", "text", "eol", "--", path]))
    }) else {
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
        let said = (git(top).ok())
            .and_then(|mut command| output(command.args(["config", "--get", name])))
            .unwrap_or_default();
        String::from_utf8_lossy(&said).trim().to_ascii_lowercase()
    };
    let autocrlf = setting("core.autocrlf");
    autocrlf == "true" || autocrlf == "input" || (specified(text) && setting("core.eol") == "crlf")
}

/// The index entries of the files at `paths` from `top`, by path. A file
/// not in the index has none.
fn list_index(top: &Path, paths: &[&str]) -> HashMap<String, Entry> {
    let mut found = HashMap::new();
    let Some(listed) = git(top).ok().and_then(|mut command| {
        output(
            command
                .args(["ls-files", "--stage", "-z", "--"])
                .args(paths),
        )
    }) else {
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
    let mut child = git(top)?
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

    /// Runs git in `top`, as a person would but with none of their own
    /// config, and checks it worked.
    fn by_hand(top: &Path, args: &[&str]) {
        let done = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(top)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(done.status.success(), "{args:?}: {done:?}");
    }

    /// A command that leaves a file called `name` in `dir` when it runs,
    /// passing what it's given through, as a filter driver would.
    fn leaves(dir: &Path, name: &str) -> String {
        format!("touch '{}'; cat", dir.join(name).display())
    }

    /// The files commands have left in `dir`, by name.
    fn left(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = (std::fs::read_dir(dir).unwrap())
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with("ran-"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_repositorys_own_filter_drivers_never_run() {
        let dir = tempfile::tempdir().unwrap();
        let top = &dir.path().canonicalize().unwrap();
        let run = |args: &[&str]| by_hand(top, args);
        run(&["init", "-q", "-b", "main"]);
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(top.join(name), "was\n").unwrap();
        }
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "a"]);
        // As a folder copied from anyone could have it: a driver for each
        // file, one called something `-c` can't name, one from a file
        // the repository's config includes, and Git LFS's, which no file
        // names here, as git-lfs may not be installed where tests run.
        let outside = tempfile::tempdir().unwrap();
        let ran = outside.path();
        let required = |name: &str| run(&["config", &format!("filter.{name}.required"), "true"]);
        run(&["config", "filter.evil.clean", &leaves(ran, "ran-clean")]);
        run(&["config", "filter.evil.smudge", &leaves(ran, "ran-smudge")]);
        required("evil");
        run(&["config", "filter.a=b.clean", &leaves(ran, "ran-equals")]);
        required("a=b");
        let included = ran.join("included");
        let driver = format!(
            "[filter \"inc\"]\n\tclean = {}\n",
            leaves(ran, "ran-included")
        );
        std::fs::write(&included, driver).unwrap();
        run(&["config", "include.path", &included.display().to_string()]);
        run(&["config", "filter.lfs.process", "git-lfs filter-process"]);
        run(&["config", "filter.lfs.clean", "git-lfs clean -- %f"]);
        let attributes = "a.txt filter=evil\nb.txt filter=a=b\nc.txt filter=inc\n";
        std::fs::write(top.join(".gitattributes"), attributes).unwrap();
        // Changed since they were added, but not in size, so git has to
        // clean them to compare them.
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(top.join(name), "new\n").unwrap();
        }
        // Git LFS's driver is left as it is; the rest are turned off.
        let command = git_filtering(top).unwrap();
        let mut named: Vec<String> = (command.get_envs())
            .filter_map(|(_, value)| value?.to_str())
            .filter_map(|key| {
                Some(
                    key.strip_prefix("filter.")?
                        .strip_suffix(".process")?
                        .to_owned(),
                )
            })
            .collect();
        named.sort();
        assert_eq!(named, ["a=b", "evil", "inc"]);

        let marks = status(top).expect("git says");
        let modified = |name: &str| (name.to_owned(), GitMark::Modified);
        assert_eq!(
            marks,
            [
                modified("a.txt"),
                modified("b.txt"),
                modified("c.txt"),
                (".gitattributes".to_owned(), GitMark::Untracked),
            ]
        );
        let files = crate::changes::changes_since_base(top, &[]).expect("git says");
        assert_eq!(files.len(), 4, "{files:?}");
        let entry = list_index(top, &["a.txt"]).remove("a.txt").unwrap();
        let object = entry.object.clone();
        let base = read_base(top, "a.txt", entry, true);
        assert_eq!(base.text.as_deref(), Some("was\n"));
        assert!(left(ran).is_empty(), "the repository's drivers ran");

        // Each would have run, without what turned it off.
        run(&["status", "--porcelain"]);
        run(&["cat-file", "--filters", "--path=a.txt", &object]);
        assert_eq!(
            left(ran),
            ["ran-clean", "ran-equals", "ran-included", "ran-smudge"]
        );
    }

    /// Makes the repository whose git directory is `git_dir` a partial
    /// clone whose promisor remote is reached over ssh by a command that
    /// leaves `ran-fetch` in `ran`, and takes `object` out of it, so git
    /// would fetch it from there.
    fn fetches_through(git_dir: &Path, ran: &Path, object: &str) {
        let run = |args: &[&str]| by_hand(git_dir, &[&["--git-dir=."], args].concat());
        run(&["config", "core.repositoryformatversion", "1"]);
        run(&["config", "extensions.partialClone", "origin"]);
        run(&["config", "remote.origin.url", "ssh://example.invalid/x"]);
        run(&["config", "remote.origin.promisor", "true"]);
        run(&["config", "protocol.allow", "always"]);
        let command = format!("touch '{}'; false", ran.join("ran-fetch").display());
        run(&["config", "core.sshCommand", &command]);
        let (fan, rest) = object.split_at(2);
        std::fs::remove_file(git_dir.join("objects").join(fan).join(rest)).unwrap();
    }

    /// Asks git, as a person would, for the text of `object` in the
    /// repository it finds in `dir`, bare or not, which makes it fetch an
    /// object that's missing.
    fn fetch_by_hand(dir: &Path, object: &str) {
        let _ = Command::new("git")
            .args(["-c", "safe.bareRepository=all", "cat-file", "-p", object])
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_NO_LAZY_FETCH")
            .env_remove("GIT_ALLOW_PROTOCOL")
            .output()
            .unwrap();
    }

    #[test]
    fn an_object_missing_from_a_partial_clone_is_never_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let top = &dir.path().canonicalize().unwrap();
        let run = |args: &[&str]| by_hand(top, args);
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(top.join("a.txt"), "was\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "a"]);
        let entry = list_index(top, &["a.txt"]).remove("a.txt").unwrap();
        let object = entry.object.clone();
        let outside = tempfile::tempdir().unwrap();
        let ran = outside.path();
        fetches_through(&top.join(".git"), ran, &object);
        std::fs::write(top.join("a.txt"), "new\n").unwrap();
        // As signs, explorers and reviews read it.
        assert_eq!(read_base(top, "a.txt", entry, false).text, None);
        assert!(status(top).is_some());
        let _ = crate::changes::changes_since_base(top, &[]);
        assert!(left(ran).is_empty(), "the repository's ssh command ran");
        // It would have run, fetching.
        fetch_by_hand(top, &object);
        assert_eq!(left(ran), ["ran-fetch"]);
    }

    #[test]
    fn a_git_directory_committed_inside_a_clone_is_no_repository() {
        let dir = tempfile::tempdir().unwrap();
        let top = &dir.path().canonicalize().unwrap();
        let run = |args: &[&str]| by_hand(top, args);
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(top.join("a.txt"), "was\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "a"]);
        let object = list_index(top, &["a.txt"]).remove("a.txt").unwrap().object;
        // A clone's folder that is itself a git directory, its work tree
        // itself, and a partial clone that fetches through a command.
        let outer = tempfile::tempdir().unwrap();
        let evil = outer.path().canonicalize().unwrap().join("evil");
        let copied = Command::new("cp")
            .arg("-r")
            .arg(top.join(".git"))
            .arg(&evil)
            .status()
            .unwrap();
        assert!(copied.success());
        by_hand(&evil, &["--git-dir=.", "config", "core.bare", "false"]);
        by_hand(&evil, &["--git-dir=.", "config", "core.worktree", "."]);
        let outside = tempfile::tempdir().unwrap();
        let ran = outside.path();
        fetches_through(&evil, ran, &object);
        assert_eq!(work_tree(&evil), None);
        assert_eq!(repository(&evil.join("a.txt")), None);
        assert_eq!(status(&evil), None);
        assert!(crate::changes::changes_since_base(&evil, &[]).is_err());
        assert!(left(ran).is_empty(), "the repository's ssh command ran");
        // It would have run, fetching.
        fetch_by_hand(&evil, &object);
        assert_eq!(left(ran), ["ran-fetch"]);
    }

    #[test]
    fn git_is_run_by_its_path_with_nothing_inherited_to_redirect_it() {
        let command = git(Path::new("/")).expect("git 2.38 or newer is installed");
        assert!(Path::new(command.get_program()).is_absolute());
        let envs: HashMap<String, Option<String>> = (command.get_envs())
            .map(|(key, value)| {
                let value = value.map(|value| value.to_string_lossy().into_owned());
                (key.to_string_lossy().into_owned(), value)
            })
            .collect();
        for removed in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CONFIG",
        ] {
            assert_eq!(envs.get(removed), Some(&None), "{removed}");
        }
        assert_eq!(envs["GIT_NO_LAZY_FETCH"].as_deref(), Some("1"));
        assert_eq!(envs["GIT_ALLOW_PROTOCOL"].as_deref(), Some(""));
        let path = envs["PATH"].clone().unwrap();
        assert!(path.split(':').all(|dir| dir.starts_with('/')), "{path}");
    }

    #[test]
    fn a_repositorys_own_hooks_never_run_as_a_hunk_is_staged() {
        let dir = tempfile::tempdir().unwrap();
        let top = &dir.path().canonicalize().unwrap();
        let run = |args: &[&str]| by_hand(top, args);
        run(&["init", "-q"]);
        let base = "1\n2\n3\n";
        std::fs::write(top.join("a.txt"), base).unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "a"]);
        // As a copied folder could have it: git runs it whenever the
        // index is written.
        let outside = tempfile::tempdir().unwrap();
        let ran = outside.path();
        let hook = top.join(".git/hooks/post-index-change");
        let script = format!("#!/bin/sh\ntouch '{}'\n", ran.join("ran-hook").display());
        std::fs::write(&hook, script).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let text = "1\ntwo\n3\n";
        let differences = diff(base, text);
        apply(top, &patch("a.txt", base, text, &differences[0])).expect("staged");
        let staged = list_index(top, &["a.txt"]).remove("a.txt").unwrap();
        let staged = read_base(top, "a.txt", staged, false);
        assert_eq!(staged.text.as_deref(), Some(text));
        assert!(left(ran).is_empty(), "the repository's hook ran");
        // It would have run, without what turned it off.
        run(&["reset", "-q"]);
        assert_eq!(left(ran), ["ran-hook"]);
    }

    #[test]
    fn a_repositorys_own_fsmonitor_command_is_never_run() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        let run = |args: &[&str]| by_hand(top, args);
        run(&["init", "-q"]);
        std::fs::write(top.join("a.txt"), "a\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "a"]);
        // As a cloned repository could have it.
        let outside = tempfile::tempdir().unwrap();
        let ran = outside.path().join("ran");
        let hook = outside.path().join("hook.sh");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        run(&["config", "core.fsmonitor", &hook.display().to_string()]);
        std::fs::write(top.join("a.txt"), "changed\n").unwrap();
        let marks = status(top).expect("git says");
        assert_eq!(marks, [("a.txt".to_owned(), GitMark::Modified)]);
        assert!(!ran.exists(), "the repository's command ran");
        // Nor as the index is listed and read, as signs and reviews do.
        assert!(list_index(top, &["a.txt"]).contains_key("a.txt"));
        assert!(!ran.exists(), "the repository's command ran");
    }

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
