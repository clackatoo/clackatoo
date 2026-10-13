//! The finder's searches, each on a thread of its own: walking a project
//! for its files, grepping them, listing the files open in buffers, and
//! reading lines of a file to preview. The event loop starts each with what
//! it needs and takes what they find, so a walk of a big project never
//! holds up a keystroke.
//!
//! What a search finds goes back in batches, each sent as soon as it's
//! full or has waited a moment, so the first results show at once and a
//! big project costs a few hundred messages, not one per file. A search
//! stops as soon as it notices it's no longer wanted: the loop drops its
//! [`Running`], when the client stops it or goes, or asks for another.
//!
//! Projects are walked as ripgrep walks them, with its own walker, on
//! several threads: leaving out what git ignores and hidden files, not
//! following links, and staying on the root's file system, so `/proc` and
//! other mounts are never walked into. Only files are read, never pipes or
//! devices, which may never have anything to read. Files are grepped whole, a file at a time, with the
//! regex crate, ripgrep's engine, skipping big files and those that aren't
//! text.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Done, Found, FoundBuffer, FoundLine};
use clackatoo_syntax::Server;
use clackatoo_text::{Diagnostic, Rope, Severity};

use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use regex::bytes::{Regex, RegexBuilder};

use crate::lsp::{self, Encoding, Reported};

/// The most files one search lists: far more than a project has, but short
/// of listing all of a home directory that has no project in it.
pub(crate) const MAX_FILES: usize = 200_000;

/// The most lines one grep finds: far more than anyone looks through, but
/// short of sending every line of a project for a pattern such as `.`.
pub(crate) const MAX_LINES: usize = 10_000;

/// Files bigger than this aren't grepped: they're data, or built, not
/// written.
const MAX_GREP_FILE: u64 = 8 * 1024 * 1024;

/// How much of a file is looked at for a NUL byte, which says it isn't
/// text, as Git and ripgrep look.
const SNIFF: usize = 8 * 1024;

/// How much of a line found is sent: enough to read, round its first match.
const MAX_TEXT: usize = 240;

/// How much of each line previewed is sent: wider than any window.
const MAX_PREVIEW_LINE: usize = 400;

/// How much of a file is read, at most, to preview it: enough to reach far
/// into a long one, but short of all of one with no lines at all.
const MAX_PREVIEW_READ: u64 = 32 * 1024 * 1024;

/// How many things a batch holds before it's sent...
const BATCH: usize = 512;

/// ...and how long it waits for more, so what's found on a slow disk still
/// shows soon.
const BATCH_WAIT: Duration = Duration::from_millis(25);

/// The most threads one walk takes.
const MAX_THREADS: usize = 8;

/// Where a search looks: the root of the project something is in.
#[derive(Clone, Debug)]
pub(crate) enum Project {
    /// A root already known, such as a language server's.
    Root(PathBuf),
    /// The project of the file at a path: its language server's root, if
    /// its language has one, or else its repository's.
    File {
        path: PathBuf,
        server: Option<Server>,
    },
    /// The project of a directory, such as a terminal's: its repository's
    /// root, or else the directory.
    Dir(PathBuf),
}

impl Project {
    /// The project's root. Directories from the home directory up aren't
    /// roots, short of the home directory itself. Outside the home
    /// directory, only a repository or a language server's project is: a
    /// directory such as `/` or `/proc` is no project to walk, and says so.
    pub(crate) fn root(&self, home: &Path) -> Result<PathBuf, String> {
        let root = match self {
            Self::Root(root) if root.parent().is_some() => return Ok(root.clone()),
            Self::Root(root) => return Err(nowhere(root)),
            Self::File {
                path,
                server: Some(server),
            } => server.root(path, home),
            Self::File { path, server: None } => {
                let dir = path.parent().unwrap_or(path);
                clackatoo_syntax::git_root(dir, home).or_else(|| Some(dir.to_owned()))
            }
            Self::Dir(dir) => clackatoo_syntax::git_root(dir, home).or_else(|| Some(dir.clone())),
        };
        let root = root.unwrap_or_else(|| home.to_owned());
        if root.starts_with(home) || root.join(".git").exists() {
            Ok(root)
        } else {
            Err(nowhere(&root))
        }
    }
}

/// Why the finder won't look in `dir`.
fn nowhere(dir: &Path) -> String {
    format!(
        "{} is in no project, and outside the home directory: the finder won't walk it",
        dir.display()
    )
}

/// What to search for.
#[derive(Debug)]
pub(crate) enum Job {
    /// The project's files.
    Files(Project),
    /// The lines in the project's files `pattern` matches.
    Grep {
        project: Project,
        pattern: String,
        exact: bool,
    },
    /// The files open in buffers, as paths from the project's root, and
    /// whether each has changes not written.
    Buffers {
        project: Project,
        buffers: Vec<(PathBuf, bool)>,
    },
    /// What's wrong in the project's files: those open, with their texts,
    /// and those not open, as their servers said, counting columns as each
    /// does.
    Diagnostics {
        project: Project,
        open: Vec<(PathBuf, Vec<Diagnostic>, Rope)>,
        elsewhere: Vec<(PathBuf, Encoding, Arc<[Reported]>)>,
    },
    /// Up to `lines` lines of the file at `path`, around line `line`, from
    /// its buffer's text if it's open.
    Preview {
        path: PathBuf,
        line: u64,
        lines: u16,
        open: Option<Rope>,
    },
}

/// The searches, and what they've found.
#[derive(Debug)]
pub(crate) struct Searches {
    home: Arc<Path>,
    send: Sender<(u64, Found)>,
    found: Receiver<(u64, Found)>,
    wake: Arc<Wake>,
    next: u64,
}

/// A search going on, which stops when this is dropped.
#[derive(Debug)]
pub(crate) struct Running {
    /// The search's number, which what it finds comes with.
    pub(crate) serial: u64,
    stop: Arc<AtomicBool>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Wakes the event loop once for any number of batches sent before it
/// takes them.
struct Wake {
    woken: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wake")
            .field("woken", &self.woken)
            .finish_non_exhaustive()
    }
}

impl Wake {
    fn wake(&self) {
        if !self.woken.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }
}

impl Searches {
    /// No searches yet. `wake` is called when one has found something, for
    /// [`take`](Self::take). Outside `home`, searches look only in
    /// projects.
    pub(crate) fn new(home: &Path, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (send, found) = mpsc::channel();
        Self {
            home: Arc::from(home),
            send,
            found,
            wake: Arc::new(Wake {
                woken: AtomicBool::new(false),
                wake: Box::new(wake),
            }),
            next: 0,
        }
    }

    /// Starts `job` on a thread of its own.
    pub(crate) fn start(&mut self, job: Job) -> Running {
        let serial = self.next;
        self.next += 1;
        let stop = Arc::new(AtomicBool::new(false));
        let out = Out {
            serial,
            send: self.send.clone(),
            wake: Arc::clone(&self.wake),
            stop: Arc::clone(&stop),
            count: AtomicUsize::new(0),
            cut_short: AtomicBool::new(false),
        };
        let home = Arc::clone(&self.home);
        let started = thread::Builder::new()
            .name("find".to_owned())
            .spawn(move || run(job, &home, &out));
        if let Err(err) = started {
            let failed = Found::Done(Done::Failed(format!("couldn't start looking: {err}")));
            if self.send.send((serial, failed)).is_ok() {
                self.wake.wake();
            }
        }
        Running { serial, stop }
    }

    /// What searches have found since last asked, each with its search's
    /// number, in the order found.
    pub(crate) fn take(&self) -> impl Iterator<Item = (u64, Found)> + '_ {
        // Cleared first, so a batch sent while these are taken wakes the
        // loop again.
        self.wake.woken.store(false, Ordering::Release);
        self.found.try_iter()
    }
}

/// Where one search sends what it finds, and how it knows to stop.
struct Out {
    serial: u64,
    send: Sender<(u64, Found)>,
    wake: Arc<Wake>,
    stop: Arc<AtomicBool>,
    /// How many things the search has found, across its threads.
    count: AtomicUsize,
    /// It stopped at its limit.
    cut_short: AtomicBool,
}

impl Out {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Sends `found`, unless the search has been stopped.
    fn send(&self, found: Found) {
        if self.stopped() {
            return;
        }
        if self.send.send((self.serial, found)).is_ok() {
            self.wake.wake();
        }
    }

    /// Counts one more thing found. Returns whether there's room for it,
    /// under `limit`; once there isn't, the search is cut short.
    fn count(&self, limit: usize) -> bool {
        if self.count.fetch_add(1, Ordering::Relaxed) < limit {
            return true;
        }
        self.cut_short.store(true, Ordering::Relaxed);
        false
    }

    /// Says the search is done, as it went.
    fn done(&self) {
        let done = if self.cut_short.load(Ordering::Relaxed) {
            Done::CutShort
        } else {
            Done::All
        };
        self.send(Found::Done(done));
    }
}

/// Carries out `job`.
fn run(job: Job, home: &Path, out: &Out) {
    match job {
        Job::Files(project) => {
            let root = match project.root(home) {
                Ok(root) => root,
                Err(why) => return out.send(Found::Done(Done::Failed(why))),
            };
            out.send(root_found(&root, home));
            walk(&root, |root| Files {
                root,
                batch: Vec::new(),
                since: Instant::now(),
                out,
            });
            out.done();
        }
        Job::Grep {
            project,
            pattern,
            exact,
        } => {
            let root = match project.root(home) {
                Ok(root) => root,
                Err(why) => return out.send(Found::Done(Done::Failed(why))),
            };
            out.send(root_found(&root, home));
            if !pattern.is_empty() {
                let regex = regex(&pattern, exact);
                walk(&root, |root| Grep {
                    root,
                    regex: &regex,
                    read: Vec::new(),
                    batch: Vec::new(),
                    since: Instant::now(),
                    out,
                });
            }
            out.done();
        }
        Job::Buffers { project, buffers } => {
            // Only to show paths from.
            let root = project.root(home).unwrap_or_else(|_| home.to_owned());
            out.send(root_found(&root, home));
            let buffers = (buffers.into_iter())
                .map(|(path, modified)| FoundBuffer {
                    path: path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                    modified,
                })
                .collect();
            out.send(Found::Buffers(buffers));
            out.done();
        }
        Job::Diagnostics {
            project,
            open,
            elsewhere,
        } => diagnostics(out, home, &project, &open, elsewhere),
        Job::Preview {
            path,
            line,
            lines,
            open,
        } => {
            let first = line.saturating_sub(u64::from(lines / 3));
            let found = match open {
                Some(text) => Ok(preview_text(&text, first, lines)),
                None => preview_file(&path, first, lines),
            };
            match found {
                Ok(lines) => {
                    out.send(Found::Preview { first, lines });
                    out.done();
                }
                Err(why) => out.send(Found::Done(Done::Failed(why))),
            }
        }
    }
}

/// Says where a search looks: at `root`, written with `~` for `home`.
fn root_found(root: &Path, home: &Path) -> Found {
    let name = match root.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => root.display().to_string(),
    };
    Found::Root {
        path: root.display().to_string(),
        name,
    }
}

/// Walks the project at `root` on several threads, each with a visitor
/// `visitor` makes, until it's done or a visitor quits, as each does once
/// its search is stopped.
fn walk<'s, V: ParallelVisitor + 's>(root: &'s Path, visitor: impl FnMut(&'s Path) -> V) {
    let threads =
        thread::available_parallelism().map_or(1, |threads| threads.get().min(MAX_THREADS));
    let walker = (WalkBuilder::new(root))
        .threads(threads)
        .same_file_system(true)
        .build_parallel();
    walker.visit(&mut Visitors { root, visitor });
}

/// Makes a visitor for each of a walk's threads.
struct Visitors<'s, F> {
    root: &'s Path,
    visitor: F,
}

impl<'s, V: ParallelVisitor + 's, F: FnMut(&'s Path) -> V> ParallelVisitorBuilder<'s>
    for Visitors<'s, F>
{
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new((self.visitor)(self.root))
    }
}

/// A file a walk came to, unless the search has been stopped: `Err` with
/// what to do next instead.
fn file_entry(entry: Result<DirEntry, ignore::Error>, out: &Out) -> Result<DirEntry, WalkState> {
    if out.stopped() {
        return Err(WalkState::Quit);
    }
    match entry {
        Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => Ok(entry),
        // A directory goes on, and one that can't be read is passed over,
        // as ripgrep passes it over.
        _ => Err(WalkState::Continue),
    }
}

/// `path`, from `root`, as text.
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// One thread's share of a walk listing files.
struct Files<'s> {
    root: &'s Path,
    batch: Vec<String>,
    since: Instant,
    out: &'s Out,
}

impl ParallelVisitor for Files<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        let entry = match file_entry(entry, self.out) {
            Ok(entry) => entry,
            Err(next) => return next,
        };
        if !self.out.count(MAX_FILES) {
            return WalkState::Quit;
        }
        self.batch.push(relative(entry.path(), self.root));
        if self.batch.len() >= BATCH || self.since.elapsed() >= BATCH_WAIT {
            self.flush();
        }
        WalkState::Continue
    }
}

impl Files<'_> {
    fn flush(&mut self) {
        self.since = Instant::now();
        if !self.batch.is_empty() {
            self.out.send(Found::Files(std::mem::take(&mut self.batch)));
        }
    }
}

impl Drop for Files<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// One thread's share of a walk grepping files.
struct Grep<'s> {
    root: &'s Path,
    regex: &'s Regex,
    /// The file being grepped, kept to read the next into.
    read: Vec<u8>,
    batch: Vec<FoundLine>,
    since: Instant,
    out: &'s Out,
}

impl ParallelVisitor for Grep<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        let entry = match file_entry(entry, self.out) {
            Ok(entry) => entry,
            Err(next) => return next,
        };
        let mut next = WalkState::Continue;
        if read_text(entry.path(), &mut self.read).is_some() {
            let path = relative(entry.path(), self.root);
            let read = std::mem::take(&mut self.read);
            grep(self.regex, &read, &mut |line| {
                if !self.out.count(MAX_LINES) {
                    next = WalkState::Quit;
                    return false;
                }
                self.batch.push(FoundLine {
                    path: path.clone(),
                    ..line
                });
                // A file with many matches goes in batches too.
                if self.batch.len() >= BATCH {
                    self.flush();
                }
                !self.out.stopped()
            });
            self.read = read;
        }
        if self.batch.len() >= BATCH || self.since.elapsed() >= BATCH_WAIT {
            self.flush();
        }
        next
    }
}

impl Grep<'_> {
    fn flush(&mut self) {
        self.since = Instant::now();
        if !self.batch.is_empty() {
            self.out.send(Found::Lines(std::mem::take(&mut self.batch)));
        }
    }
}

impl Drop for Grep<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

/// `pattern` as a regular expression, matching case exactly if `exact`, or
/// as plain text if it isn't a valid one, such as `(` typed on the way to
/// `(a|b)`. `^` and `$` match at each line's start and end.
fn regex(pattern: &str, exact: bool) -> Regex {
    let build = |pattern: &str| {
        RegexBuilder::new(pattern)
            .case_insensitive(!exact)
            .multi_line(true)
            .crlf(true)
            .build()
    };
    build(pattern)
        .or_else(|_| build(&regex::escape(pattern)))
        .expect("plain text is a valid regular expression")
}

/// Sends what's wrong in `open` files and `elsewhere`, the worst first,
/// then by file and place, as lines from the project's root, each saying
/// what's wrong as the bar does. A file not open is read to place its
/// columns; one that can't be is placed at its lines' starts.
fn diagnostics(
    out: &Out,
    home: &Path,
    project: &Project,
    open: &[(PathBuf, Vec<Diagnostic>, Rope)],
    elsewhere: Vec<(PathBuf, Encoding, Arc<[Reported]>)>,
) {
    let root = project.root(home).unwrap_or_else(|_| home.to_owned());
    out.send(root_found(&root, home));
    let mut found: Vec<(Severity, String, u64, usize, String)> = Vec::new();
    let shown = |path: &Path| {
        path.strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let at = |text: &Rope, byte: usize| {
        let byte = byte.min(text.len_bytes());
        let line = text.byte_to_line(byte);
        (line as u64, byte - text.line_to_byte(line))
    };
    for (path, diagnostics, text) in open {
        if !path.starts_with(&root) {
            continue;
        }
        for diagnostic in diagnostics {
            let (line, col) = at(text, diagnostic.start);
            let said = diagnostic.described();
            found.push((diagnostic.severity, shown(path), line, col, said));
        }
    }
    let mut read = Vec::new();
    for (path, encoding, reported) in elsewhere {
        if out.stopped() {
            return;
        }
        if !path.starts_with(&root) {
            continue;
        }
        let text =
            (read_text(&path, &mut read)).map(|()| Rope::from_str(&String::from_utf8_lossy(&read)));
        for reported in reported.iter().cloned() {
            let first = u64::from(reported.start.line);
            let (diagnostic, (line, col)) = match &text {
                Some(text) => {
                    let diagnostic = lsp::diagnostic(text, reported, encoding);
                    let place = at(text, diagnostic.start);
                    (diagnostic, place)
                }
                None => (
                    lsp::diagnostic(&Rope::new(), reported, encoding),
                    (first, 0),
                ),
            };
            let said = diagnostic.described();
            found.push((diagnostic.severity, shown(&path), line, col, said));
        }
    }
    found.sort_by(|a, b| (a.0, &a.1, a.2, a.3).cmp(&(b.0, &b.1, b.2, b.3)));
    found.truncate(MAX_LINES);
    for batch in found.chunks(BATCH) {
        let lines = (batch.iter())
            .map(|(_, path, line, col, text)| FoundLine {
                path: path.clone(),
                line: *line,
                col: *col,
                text: text.clone(),
                matches: Vec::new(),
            })
            .collect();
        out.send(Found::Lines(lines));
    }
    out.done();
}

/// Reads the file at `path` into `into`, if it's text and not too big.
fn read_text(path: &Path, into: &mut Vec<u8>) -> Option<()> {
    into.clear();
    let file = open_file(path).ok()?;
    // Not by its size, which a file in /proc says is 0.
    file.take(MAX_GREP_FILE + 1).read_to_end(into).ok()?;
    if into.len() as u64 > MAX_GREP_FILE {
        return None;
    }
    let sniffed = &into[..into.len().min(SNIFF)];
    (memchr::memchr(0, sniffed).is_none()).then_some(())
}

/// Opens the file at `path` to read, if it's a file: not a pipe or a
/// device, which a read might wait on for ever, or a directory. It's opened
/// without waiting, as a pipe would have it wait for a writer.
fn open_file(path: &Path) -> io::Result<File> {
    let nonblocking = i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap_or_default();
    let file = (OpenOptions::new())
        .read(true)
        .custom_flags(nonblocking)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("it isn't a file"));
    }
    Ok(file)
}

/// Passes each line of `text` that `regex` matches, with an empty path, to
/// `found`, until it returns false.
fn grep(regex: &Regex, text: &[u8], found: &mut dyn FnMut(FoundLine) -> bool) {
    let mut at = 0;
    // Lines counted so far, up to the start of line `line`.
    let (mut line, mut counted) = (0, 0);
    // Not from the end: that's no line, though `^` matches there.
    while at < text.len() {
        let Some(first) = regex.find_at(text, at) else {
            break;
        };
        let start = memchr::memrchr(b'\n', &text[..first.start()]).map_or(0, |end| end + 1);
        let end = memchr::memchr(b'\n', &text[first.start()..])
            .map_or(text.len(), |end| first.start() + end);
        line += memchr::memchr_iter(b'\n', &text[counted..start]).count();
        counted = start;
        // Matched again within the line alone, so a match running onto the
        // next line counts for neither.
        let whole = &text[start..end];
        let whole = whole.strip_suffix(b"\r").unwrap_or(whole);
        let matches: Vec<(usize, usize)> = (regex.find_iter(whole))
            .map(|matched| (matched.start(), matched.end()))
            .collect();
        if let Some(&(col, _)) = matches.first()
            && !found(found_line(whole, line as u64, col, &matches))
        {
            return;
        }
        at = end + 1;
    }
}

/// Line `line` of a file, `whole`, whose first match starts `col` bytes in,
/// with its matches, cut short round them.
fn found_line(whole: &[u8], line: u64, col: usize, matches: &[(usize, usize)]) -> FoundLine {
    // From a little before the first match, if it's far along.
    let from = if col > MAX_TEXT / 2 {
        boundary(whole, col - MAX_TEXT / 4)
    } else {
        0
    };
    let to = boundary(whole, (from + MAX_TEXT).min(whole.len()));
    let (text, matches) = match std::str::from_utf8(&whole[from..to]) {
        Ok(text) => {
            let matches = (matches.iter())
                .filter(|&&(start, end)| start >= from && end <= to && start < end)
                .map(|&(start, end)| (start - from, end - from))
                .collect();
            (text.to_owned(), matches)
        }
        // Not text a match's place in can be trusted.
        Err(_) => (
            String::from_utf8_lossy(&whole[from..to]).into_owned(),
            Vec::new(),
        ),
    };
    FoundLine {
        path: String::new(),
        line,
        col,
        text,
        matches,
    }
}

/// The start of the character byte `at` of `text` is in, if it's UTF-8.
fn boundary(text: &[u8], mut at: usize) -> usize {
    while at > 0 && at < text.len() && (text[at] & 0b1100_0000) == 0b1000_0000 {
        at -= 1;
    }
    at
}

/// `text` cut to at most `max` bytes, at a character's start.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let at = boundary(text.as_bytes(), max);
        text.truncate(at);
    }
    text
}

/// Up to `count` lines of `text` from line `first`.
fn preview_text(text: &Rope, first: u64, count: u16) -> Vec<String> {
    let first = usize::try_from(first).unwrap_or(usize::MAX);
    let lines = text.len_lines();
    (first..lines.min(first.saturating_add(usize::from(count))))
        .map(|line| {
            let line = text.line(line).to_string();
            let line = line.strip_suffix('\n').unwrap_or(&line);
            cut(line.to_owned(), MAX_PREVIEW_LINE)
        })
        .collect()
}

/// Up to `count` lines of the file at `path` from line `first`, read no
/// further than they go, or why there are none.
fn preview_file(path: &Path, first: u64, count: u16) -> Result<Vec<String>, String> {
    let file = open_file(path).map_err(|err| format!("can't read it: {err}"))?;
    let mut reader = BufReader::new(file.take(MAX_PREVIEW_READ));
    let start = reader
        .fill_buf()
        .map_err(|err| format!("can't read it: {err}"))?;
    if memchr::memchr(0, &start[..start.len().min(SNIFF)]).is_some() {
        return Err("it isn't text".to_owned());
    }
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut number = 0;
    while lines.len() < usize::from(count) {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if number >= first {
            let text = line.strip_suffix(b"\n").unwrap_or(&line);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            let text = &text[..boundary(text, text.len().min(MAX_PREVIEW_LINE))];
            lines.push(String::from_utf8_lossy(text).into_owned());
        }
        number += 1;
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of `text` `pattern` matches.
    fn all(pattern: &str, exact: bool, text: &str) -> Vec<FoundLine> {
        let mut found = Vec::new();
        grep(&regex(pattern, exact), text.as_bytes(), &mut |line| {
            found.push(line);
            true
        });
        found
    }

    /// A line found: its number, where its first match is, its text, and
    /// where its matches are.
    type Line = (u64, usize, String, Vec<(usize, usize)>);

    fn lines(pattern: &str, exact: bool, text: &str) -> Vec<Line> {
        (all(pattern, exact, text).into_iter())
            .map(|found| (found.line, found.col, found.text, found.matches))
            .collect()
    }

    #[test]
    fn grep_finds_each_line_once_with_every_match_in_it() {
        let text = "one fish\ntwo fish fish\r\nred\nFISH\n";
        assert_eq!(
            lines("fish", false, text),
            [
                (0, 4, "one fish".into(), vec![(4, 8)]),
                (1, 4, "two fish fish".into(), vec![(4, 8), (9, 13)]),
                (3, 0, "FISH".into(), vec![(0, 4)]),
            ]
        );
        assert_eq!(lines("fish", true, text).len(), 2, "case matched exactly");
        // Anchors are at lines' ends, a CR not counting.
        assert_eq!(lines("^t", true, text)[0].0, 1);
        assert_eq!(lines("fish$", true, text).len(), 2);
        // A match running over a line's end counts for neither line.
        assert!(lines("fish\\s+two", true, text).is_empty());
        // What isn't a valid regular expression is plain text.
        assert_eq!(lines("(", true, "a(b\nc")[0].2, "a(b");
        // An empty match still matches the line, but there's no line after
        // the last line's end.
        assert_eq!(lines("x*", true, "a\nb").len(), 2);
        assert_eq!(lines("^", true, "a\nb\n").len(), 2);
        assert_eq!(lines("$", true, "a\n\n").len(), 2);
        assert!(lines("^", true, "").is_empty());
    }

    #[test]
    fn a_long_line_is_cut_short_round_its_first_match() {
        let long = format!("{}needle{}", "é".repeat(300), "z".repeat(500));
        let found = &all("needle", true, &long)[0];
        assert_eq!(found.col, 600);
        let (start, end) = found.matches[0];
        assert_eq!(&found.text[start..end], "needle");
        assert!(found.text.len() <= MAX_TEXT);
    }

    #[test]
    fn previews_come_from_a_buffer_or_the_file_and_not_from_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let text = (0..100).fold(String::new(), |text, n| text + &format!("line {n}\n"));
        let path = dir.path().join("a");
        std::fs::write(&path, &text).unwrap();
        let from_file = preview_file(&path, 10, 3).unwrap();
        assert_eq!(from_file, ["line 10", "line 11", "line 12"]);
        assert_eq!(preview_text(&Rope::from_str(&text), 10, 3), from_file);
        assert_eq!(preview_file(&path, 99, 3).unwrap(), ["line 99"]);
        std::fs::write(&path, b"ELF\0\x01").unwrap();
        assert_eq!(preview_file(&path, 0, 3), Err("it isn't text".into()));
        // A pipe, which nothing writes, isn't waited on, nor a directory.
        let pipe = dir.path().join("pipe");
        rustix::fs::mkfifoat(rustix::fs::CWD, &pipe, rustix::fs::Mode::RWXU).unwrap();
        assert!(preview_file(&pipe, 0, 3).is_err());
        assert!(read_text(&pipe, &mut Vec::new()).is_none());
        assert!(preview_file(dir.path(), 0, 3).is_err());
        // A file says how big it is by what's read, not what it's said.
        let mut read = Vec::new();
        assert!(read_text(Path::new("/proc/self/status"), &mut read).is_some());
        assert!(!read.is_empty());
    }

    #[test]
    fn a_search_dropped_stops_and_says_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        for sub in 0..100 {
            let sub = dir.path().join(sub.to_string());
            std::fs::create_dir(&sub).unwrap();
            for file in 0..100 {
                std::fs::write(sub.join(file.to_string()), "").unwrap();
            }
        }
        let mut searches = Searches::new(dir.path(), || {});
        let running = searches.start(Job::Files(Project::Root(dir.path().to_owned())));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !searches
            .take()
            .any(|(_, found)| matches!(found, Found::Files(_)))
        {
            assert!(Instant::now() < deadline, "nothing was found");
            thread::sleep(Duration::from_millis(1));
        }
        drop(running);
        // Every thread of the walk lets go of what they shared once it's
        // over.
        while Arc::strong_count(&searches.wake) > 1 {
            assert!(Instant::now() < deadline, "the search went on");
            thread::sleep(Duration::from_millis(1));
        }
        let after: Vec<Found> = searches.take().map(|(_, found)| found).collect();
        assert!(
            !after.iter().any(|found| matches!(found, Found::Done(_))),
            "{after:?}"
        );
    }

    #[test]
    fn a_project_is_its_repository_or_its_directory_but_never_all_of_home() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::create_dir_all(home.join("notes")).unwrap();
        let dir = |path: &str| Project::Dir(home.join(path)).root(home).unwrap();
        assert_eq!(dir("repo/src/deep"), repo);
        assert_eq!(dir("notes"), home.join("notes"));
        assert_eq!(dir(""), home);
        let file = Project::File {
            path: repo.join("src/a.txt"),
            server: None,
        };
        assert_eq!(file.root(home).unwrap(), repo);
        let root = Project::Root(home.join("x")).root(home);
        assert_eq!(root.unwrap(), home.join("x"));
        // Outside home, a repository is a project, and nothing else is.
        let elsewhere = tempfile::tempdir().unwrap();
        let elsewhere = elsewhere.path();
        std::fs::create_dir_all(elsewhere.join("app/.git")).unwrap();
        std::fs::create_dir_all(elsewhere.join("app/src")).unwrap();
        let outside = |dir: &Path| Project::Dir(dir.to_owned()).root(home);
        assert_eq!(
            outside(&elsewhere.join("app/src")).unwrap(),
            elsewhere.join("app")
        );
        assert!(outside(elsewhere).unwrap_err().contains("in no project"));
        assert!(outside(Path::new("/proc")).is_err());
        assert!(Project::Root("/".into()).root(home).is_err());
        assert_eq!(
            root_found(&repo, home),
            Found::Root {
                path: repo.display().to_string(),
                name: "~/repo".into()
            }
        );
    }
}
