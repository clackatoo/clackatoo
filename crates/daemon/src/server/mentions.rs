//! The files a message to a chat mentions, `@path` (CLACKA-193; see
//! [`clackatoo_protocol::mentions`]), sent to its agent as ACP context:
//! each that names a file in the chat's project as a `resource_link`, a
//! link to the file, or as an embedded `resource`, the file's text.
//!
//! What both adapters make of them, as their sources show
//! (claude-agent-acp 0.98 `promptToClaude`, codex-acp 2.2.2
//! `buildPromptItems`): a link becomes the words `[@name](file:///…)`, and
//! nothing of the file reaches the model unless it reads it; an embedded
//! text becomes the link and `<context ref="file:///…">TEXT</context>`
//! (Claude's after the whole message, Codex's in place). Neither reads a
//! range of lines from anywhere but the URI, so lines go in it, as
//! `#L12-L20`, which the model sees in the link and the `ref`.
//!
//! So what's sent is what the model should see:
//!
//! - Lines of a file (an editor's selection, see `:Ask`), and a file open
//!   with changes not saved, go as their text, the buffer's as it is now,
//!   so the agent sees what the user sees.
//! - A saved file goes as its text only to agents better for it (see
//!   [`crate::agents::embeds_saved`]): Codex, which has no read tool of
//!   its own but the shell. Claude Code gets a link: it edits only what
//!   its Read tool has read, so text it was given would be read again
//!   before any edit, and its Read gives line numbers.
//! - Only to an agent that says it takes them (`embeddedContext`); any
//!   other gets links.
//! - A file over [`MOST_FILE`], or past [`MOST_EMBEDDED`] in all, or not
//!   UTF-8, goes as a link. So does a folder.
//!
//! A mention of something outside the chat's project (as CLACKA-178 draws
//! it) stays words, and the chat says so; one that names nothing is
//! words. It's judged first without the disk, by the path as written made
//! absolute, so a path plainly outside is never looked at; then again with
//! links followed. A file is read through one handle, opened without
//! following a link or waiting on a pipe: it must be a plain file, under
//! the project as the handle says, and no more is read of it than is to
//! be sent, so nothing swapped in after the check is read. A slash
//! command's message is read for none, as the adapters look for the
//! command in its last or only block.
//!
//! The disk is read on a thread of the message's own, never the event
//! loop: the message, and what its chat asks after it, wait for it (see
//! [`Daemon::act`]), so nothing overtakes it; but Stop, and answers to
//! the agent, don't wait, and Stop drops the message. A thread a job
//! each, rather than one for all, as a file system that hangs one read
//! mustn't hold another chat's: a job is rare, one a message that
//! mentions something, and at most [`MOST_READING`] go at once. One that
//! takes longer than [`DEADLINE`] is given up on: the message goes
//! without its files, and the chat says so; its thread ends whenever the
//! file system lets it, and what it read is thrown away.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agent_client_protocol_schema::v1::StopReason;
use clackatoo_protocol::{TerminalId, lines_suffix, mention, mentions, slash_command};
use clackatoo_text::Rope;
use serde_json::{Value, json};

use super::Daemon;
use crate::chat::Action;
use crate::find::lexical;

/// The most of a file sent as its text: more goes as a link.
pub(crate) const MOST_FILE: usize = 64 * 1024;

/// The most text a message's mentions send in all: those past it go as
/// links.
pub(crate) const MOST_EMBEDDED: usize = 256 * 1024;

/// The most of a file not open read for lines of it: those of a bigger
/// one go as a link to them.
const MOST_READ_FOR_LINES: usize = 8 * 1024 * 1024;

/// How long a message waits for the files it mentions to be read.
pub(crate) const DEADLINE: Duration = Duration::from_secs(2);

/// The most reads going at once, those stuck on a file system that hangs
/// included, until they return: past it, a message goes without its
/// files.
const MOST_READING: usize = 8;

/// What's to be read for a message's mentions, gathered on the loop.
#[derive(Debug)]
pub(crate) struct Job {
    /// Each mention: its path as written, and its lines.
    pub(crate) mentions: Vec<(String, Option<(u32, u32)>)>,
    /// The chat's working directory and project, made absolute with links
    /// followed.
    pub(crate) cwd: PathBuf,
    pub(crate) project: PathBuf,
    pub(crate) home: PathBuf,
    /// The buffers open for files mentioned: path, whether changed since
    /// saved, and text.
    pub(crate) buffers: Vec<(PathBuf, bool, Rope)>,
    /// Whether the agent takes text, and saved files' text.
    pub(crate) embed: bool,
    pub(crate) embed_saved: bool,
}

/// What reading a message's mentions made.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Read {
    /// The blocks to send after the message's text.
    pub(crate) blocks: Vec<Value>,
    /// The mentions of something outside the project, as written.
    pub(crate) outside: Vec<String>,
}

/// What a read is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum For {
    /// The message its chat holds (see [`Held`]).
    Message,
    /// Steering the chat's queued message with this ID: with its words, as
    /// steering sends, unless it mentions a file, when the turn is
    /// stopped and it's sent whole.
    Steer(u64),
}

/// A read going on.
#[derive(Debug)]
struct Reading {
    job: u64,
    chat: TerminalId,
    purpose: For,
    /// When it's given up on.
    due: Instant,
    /// What the message mentions, as written, to say what wasn't read.
    mentioned: Vec<String>,
}

/// A chat's message waiting for what it mentions to be read, and what the
/// chat asked after it.
#[derive(Debug)]
struct Held {
    job: u64,
    message: Action,
    after: Vec<Action>,
}

/// The messages waiting for their mentions to be read.
#[derive(Debug)]
pub(super) struct Mentioning {
    next: u64,
    reading: Vec<Reading>,
    held: HashMap<TerminalId, Held>,
    /// What threads have read, by job.
    done: Arc<Mutex<Vec<(u64, Read)>>>,
    /// The threads still going, those given up on included.
    alive: Arc<AtomicUsize>,
    /// How long a read may take: [`DEADLINE`], but for tests.
    deadline: Duration,
    /// What reads a job: [`read`], but for tests.
    reader: fn(&Job) -> Read,
}

impl Default for Mentioning {
    fn default() -> Self {
        Self {
            next: 0,
            reading: Vec::new(),
            held: HashMap::new(),
            done: Arc::default(),
            alive: Arc::default(),
            deadline: DEADLINE,
            reader: read,
        }
    }
}

impl Mentioning {
    /// Reads `job` for `purpose`, for the chat `chat`, on a thread of its
    /// own, waking the loop with `waker` once it's read. Returns its
    /// number, or `None` if it can't be read now.
    fn submit(
        &mut self,
        (chat, purpose): (TerminalId, For),
        job: Job,
        waker: &Arc<mio::Waker>,
    ) -> Option<u64> {
        if self.alive.load(Ordering::Acquire) >= MOST_READING {
            log::warn!("{MOST_READING} reads of mentioned files are going: no more for now");
            return None;
        }
        self.next += 1;
        let number = self.next;
        let mentioned = (job.mentions.iter())
            .map(|(path, lines)| mention(path, *lines))
            .collect();
        let (done, alive, waker) = (
            Arc::clone(&self.done),
            Arc::clone(&self.alive),
            Arc::clone(waker),
        );
        let reader = self.reader;
        alive.fetch_add(1, Ordering::AcqRel);
        let spawned = std::thread::Builder::new()
            .name("mentions".into())
            .spawn(move || {
                let read = reader(&job);
                (done.lock().unwrap_or_else(PoisonError::into_inner)).push((number, read));
                alive.fetch_sub(1, Ordering::AcqRel);
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for mentions failed: {err}");
                }
            });
        if let Err(err) = spawned {
            log::error!("a thread to read mentioned files couldn't start: {err}");
            self.alive.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        self.reading.push(Reading {
            job: number,
            chat,
            purpose,
            due: Instant::now() + self.deadline,
            mentioned,
        });
        Some(number)
    }
}

impl Daemon {
    /// Of `actions`, asked by the chat in `id`, those to carry out now:
    /// all of them, unless a message of its waits for what it mentions to
    /// be read. Then the rest wait behind it, but for answers to the
    /// agent, which go at once, and Stop, which drops the message, ends
    /// its turn as stopped, and lets what waited behind it go.
    pub(super) fn not_waiting(&mut self, id: TerminalId, actions: Vec<Action>) -> Vec<Action> {
        let Some(held) = self.mentioning.held.get_mut(&id) else {
            return actions;
        };
        let mut now = Vec::new();
        let mut actions = actions.into_iter();
        let mut stop = None;
        for action in actions.by_ref() {
            match action {
                Action::Cancel | Action::Abandon => {
                    stop = Some(action);
                    break;
                }
                Action::Answer { .. } | Action::Elicited { .. } | Action::Permit { .. } => {
                    now.push(action);
                }
                other => held.after.push(other),
            }
        }
        let Some(stop) = stop else {
            return now;
        };
        let held = self.mentioning.held.remove(&id).expect("held above");
        self.mentioning
            .reading
            .retain(|reading| reading.job != held.job);
        if let Some(chat) = self.chat(id) {
            chat.answered(Ok(StopReason::Cancelled));
            self.settle_turn(id);
            self.collect_damage(id);
        }
        // The message's turn is stopped already: Stop needn't reach the
        // agent, which never had it.
        if matches!(stop, Action::Abandon) {
            now.push(stop);
        }
        now.extend(held.after);
        now.extend(actions);
        now
    }

    /// The blocks to send after `action`'s text, a message to the chat in
    /// `id`: none, unless it mentions files. Then it starts reading them,
    /// and returns `Err`, holding the action, and what follows it in
    /// `after`, until they're read.
    pub(super) fn mentioned(
        &mut self,
        id: TerminalId,
        action: Action,
        after: &mut std::vec::IntoIter<Action>,
    ) -> Result<(Action, Vec<Value>), ()> {
        let text = match &action {
            Action::Prompt(prompt)
            | Action::Briefed { text: prompt, .. }
            | Action::Carry { text: prompt, .. } => &prompt.text,
            _ => return Ok((action, Vec::new())),
        };
        let Some(job) = self.mention_job(id, text) else {
            return Ok((action, Vec::new()));
        };
        let Some(number) = self.mentioning.submit((id, For::Message), job, &self.waker) else {
            if let Some(chat) = self.chat(id) {
                chat.notify("too many files are being read: the agent was sent the words".into());
            }
            return Ok((action, Vec::new()));
        };
        let held = Held {
            job: number,
            message: action,
            after: after.by_ref().collect(),
        };
        self.mentioning.held.insert(id, held);
        Err(())
    }

    /// Reads what `text`, the chat in `id`'s queued message `message`,
    /// mentions, to steer with it as [`steer_read`](Self::steer_read)
    /// says. Returns whether it's being read.
    pub(super) fn read_for_steering(&mut self, id: TerminalId, message: u64, text: &str) -> bool {
        let Some(job) = self.mention_job(id, text) else {
            return false;
        };
        (self
            .mentioning
            .submit((id, For::Steer(message)), job, &self.waker))
        .is_some()
    }

    /// What's to be read for `text`, a message to the chat in `id`, if it
    /// mentions anything and isn't a slash command.
    fn mention_job(&mut self, id: TerminalId, text: &str) -> Option<Job> {
        if slash_command(text).is_some() {
            return None;
        }
        let found = mentions(text);
        if found.is_empty() {
            return None;
        }
        let home = self.config.home.clone();
        let agent = self.agent_of(id);
        let embed = (agent.and_then(|agent| self.agents_can_embed(agent))).unwrap_or(false);
        let chat = self.chat(id)?;
        let cwd = chat.real_cwd();
        let project = chat.project().to_owned();
        let embed_saved = crate::agents::embeds_saved(chat.agent());
        let mentions: Vec<(String, Option<(u32, u32)>)> = (found.iter())
            .map(|found| (found.path.to_owned(), found.lines))
            .collect();
        let places: HashSet<PathBuf> = (mentions.iter())
            .flat_map(|(path, _)| places(path, &cwd, &project, &home))
            .collect();
        let buffers = (self.buffers.values())
            .filter(|slot| places.contains(slot.buffer.path()))
            .map(|slot| {
                let buffer = &slot.buffer;
                (
                    buffer.path().to_owned(),
                    buffer.modified(),
                    buffer.text().clone(),
                )
            })
            .collect();
        Some(Job {
            mentions,
            cwd,
            project,
            home,
            buffers,
            embed,
            embed_saved,
        })
    }

    /// When the next read is given up on, if one is going.
    pub(super) fn next_mentions_due(&self) -> Option<Instant> {
        self.mentioning
            .reading
            .iter()
            .map(|reading| reading.due)
            .min()
    }

    /// Sends the messages whose mentions have been read, or whose reads
    /// have taken too long, and what was held behind them; and steers with
    /// those read for steering.
    pub(super) fn take_mentions(&mut self) {
        let done = std::mem::take(
            &mut *(self.mentioning.done.lock()).unwrap_or_else(PoisonError::into_inner),
        );
        for (job, read) in done {
            // One given up on, or whose message was stopped, is thrown away.
            let Some(at) = (self.mentioning.reading.iter()).position(|reading| reading.job == job)
            else {
                continue;
            };
            let reading = self.mentioning.reading.remove(at);
            self.mentions_read(&reading, Some(read));
        }
        let now = Instant::now();
        while let Some(at) = (self.mentioning.reading.iter()).position(|reading| reading.due <= now)
        {
            let reading = self.mentioning.reading.remove(at);
            log::warn!("reading {:?} took too long: given up on", reading.mentioned);
            self.mentions_read(&reading, None);
        }
    }

    /// Goes on with what `reading` was for, now that what it read is
    /// `read`, or it was given up on.
    fn mentions_read(&mut self, reading: &Reading, read: Option<Read>) {
        let id = reading.chat;
        let notice = match &read {
            Some(read) if !read.outside.is_empty() => Some(format!(
                "{} isn't in the chat's project, so the agent was sent the words, not the file",
                read.outside.join(", ")
            )),
            Some(_) => None,
            None => Some(format!(
                "couldn't read {} in time, so the agent was sent the words",
                reading.mentioned.join(", ")
            )),
        };
        if let Some(notice) = notice
            && let Some(chat) = self.chat(id)
        {
            chat.notify(notice);
            self.collect_damage(id);
        }
        let blocks = read.map(|read| read.blocks).unwrap_or_default();
        match reading.purpose {
            For::Message => {
                let Some(held) =
                    (self.mentioning.held.remove(&id)).filter(|held| held.job == reading.job)
                else {
                    return;
                };
                let actions = std::iter::once(held.message).chain(held.after).collect();
                self.act_with(id, actions, Some(blocks));
            }
            For::Steer(message) => self.steer_read(id, message, !blocks.is_empty()),
        }
    }
}

/// Where `path`, mentioned in a chat working in `cwd` in `project`, may
/// be, as written, made absolute, in the order looked: from the working
/// directory, as the agent reads it, then from the project's root, as the
/// window's completion offers it.
fn places(path: &str, cwd: &Path, project: &Path, home: &Path) -> Vec<PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return vec![lexical(&home.join(rest))];
    }
    let written = Path::new(path);
    if written.is_absolute() {
        return vec![lexical(written)];
    }
    vec![lexical(&cwd.join(written)), lexical(&project.join(written))]
}

/// Reads what `job` mentions, as the module's docs say.
pub(crate) fn read(job: &Job) -> Read {
    let mut made = Read::default();
    let root = &job.project;
    let mut budget = MOST_EMBEDDED;
    let mut seen = HashSet::new();
    for (written, lines) in &job.mentions {
        // Plainly outside, as written: the disk isn't looked at.
        let candidates: Vec<PathBuf> = places(written, &job.cwd, root, &job.home)
            .into_iter()
            .filter(|place| place.starts_with(root))
            .collect();
        let real = candidates
            .iter()
            .find_map(|place| place.canonicalize().ok());
        let outside = match &real {
            None => candidates.is_empty(),
            Some(real) => !real.starts_with(root),
        };
        if outside {
            let shown = mention(written, *lines);
            if !made.outside.contains(&shown) {
                made.outside.push(shown);
            }
            continue;
        }
        let Some(real) = real else { continue };
        if !seen.insert((real.clone(), *lines)) {
            continue;
        }
        let Ok(mut uri) = url::Url::from_file_path(&real) else {
            continue;
        };
        let suffix = lines_suffix(*lines);
        if let Some(fragment) = suffix.strip_prefix('#') {
            uri.set_fragment(Some(fragment));
        }
        let text = if job.embed {
            text_of(job, &real, *lines)
        } else {
            None
        };
        let text = text.filter(|text| text.len() <= MOST_FILE && text.len() <= budget);
        let name = format!("{written}{suffix}");
        made.blocks.push(match text {
            Some(text) => {
                budget -= text.len();
                json!({
                    "type": "resource",
                    "resource": { "uri": uri.as_str(), "mimeType": "text/plain", "text": text },
                })
            }
            None => json!({ "type": "resource_link", "uri": uri.as_str(), "name": name }),
        });
    }
    made
}

/// The text to send of the file at `real`, or of its `lines`, if it's to
/// go as text: its buffer's, if it's open, or else what's on the disk.
fn text_of(job: &Job, real: &Path, lines: Option<(u32, u32)>) -> Option<String> {
    let buffer = (job.buffers.iter()).find(|(path, ..)| path == real);
    let root = &job.project;
    match (lines, buffer) {
        (Some((first, last)), Some((_, _, rope))) => lines_of(&rope.to_string(), first, last),
        (Some((first, last)), None) => {
            let bytes = read_under(real, root, MOST_READ_FOR_LINES)?;
            lines_of(&String::from_utf8(bytes).ok()?, first, last)
        }
        (None, Some((_, true, rope))) => (rope.len_bytes() <= MOST_FILE).then(|| rope.to_string()),
        (None, _) if job.embed_saved => String::from_utf8(read_under(real, root, MOST_FILE)?).ok(),
        (None, _) => None,
    }
}

/// The file at `real`, at most `most` bytes of it, if it's a plain file
/// under `root` as its handle says, and no bigger: opened once, without
/// following a link or waiting on a pipe, so what's read is what was
/// checked.
fn read_under(real: &Path, root: &Path, most: usize) -> Option<Vec<u8>> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(real)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let at = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    if !at.starts_with(root) {
        return None;
    }
    let mut bytes = Vec::new();
    let most = u64::try_from(most).unwrap_or(u64::MAX);
    file.take(most.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= most).then_some(bytes)
}

/// Lines `first` to `last` of `text`, from 1, with their line ends; those
/// past its end left out, and none if `first` is.
fn lines_of(text: &str, first: u32, last: u32) -> Option<String> {
    let first = usize::try_from(first).ok()?.checked_sub(1)?;
    let count = usize::try_from(last).ok()?.checked_sub(first)?;
    let lines: String = text.split_inclusive('\n').skip(first).take(count).collect();
    (!lines.is_empty()).then_some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(dir: &Path, mentions: &[(&str, Option<(u32, u32)>)]) -> Job {
        let dir = dir.canonicalize().unwrap();
        Job {
            mentions: (mentions.iter())
                .map(|&(path, lines)| (path.to_owned(), lines))
                .collect(),
            cwd: dir.join("project/sub"),
            project: dir.join("project"),
            home: dir.clone(),
            buffers: Vec::new(),
            embed: true,
            embed_saved: true,
        }
    }

    fn files(dir: &Path) {
        fs::create_dir_all(dir.join("project/sub")).unwrap();
        fs::write(dir.join("project/a.rs"), "one\ntwo\nthree\n").unwrap();
        fs::write(dir.join("project/sub/b.rs"), "b\n").unwrap();
        fs::write(dir.join("outside.rs"), "secret\n").unwrap();
        std::os::unix::fs::symlink(dir.join("outside.rs"), dir.join("project/link.rs")).unwrap();
    }

    fn kinds(read: &Read) -> Vec<(&str, &str)> {
        (read.blocks.iter())
            .map(|block| {
                let kind = block["type"].as_str().unwrap();
                let uri = (block.pointer("/resource/uri").or(block.get("uri")))
                    .and_then(Value::as_str)
                    .unwrap();
                (kind, uri.rsplit('/').next().unwrap())
            })
            .collect()
    }

    #[test]
    fn files_in_the_project_go_as_text_or_links_and_others_are_words() {
        let dir = tempfile::tempdir().unwrap();
        files(dir.path());
        let mut job = job(
            dir.path(),
            &[
                ("b.rs", None),
                ("a.rs", Some((2, 3))),
                ("../a.rs", None),
                ("~/outside.rs", None),
                ("link.rs", None),
                ("missing.rs", None),
                ("/etc/hostname", None),
                ("b.rs", None),
            ],
        );
        let read = read(&job);
        assert_eq!(
            kinds(&read),
            [
                ("resource", "b.rs"),
                ("resource", "a.rs#L2-L3"),
                ("resource", "a.rs")
            ]
        );
        assert_eq!(read.blocks[1]["resource"]["text"], "two\nthree\n");
        assert_eq!(
            read.outside,
            ["@~/outside.rs", "@link.rs", "@/etc/hostname"]
        );
        // To Claude, saved files are links; lines are still their text.
        job.embed_saved = false;
        let read = super::read(&job);
        assert_eq!(read.blocks[0]["type"], "resource_link");
        assert_eq!(read.blocks[0]["name"], "b.rs");
        assert_eq!(read.blocks[1]["type"], "resource");
        // An agent that takes no text gets links alone.
        job.embed = false;
        let read = super::read(&job);
        assert!(
            read.blocks
                .iter()
                .all(|block| block["type"] == "resource_link")
        );
    }

    #[test]
    fn a_buffer_not_saved_goes_as_it_is_now_and_big_files_as_links() {
        let dir = tempfile::tempdir().unwrap();
        files(dir.path());
        let path = dir.path().join("project/a.rs").canonicalize().unwrap();
        fs::write(
            dir.path().join("project/big.txt"),
            "x".repeat(MOST_FILE + 1),
        )
        .unwrap();
        let mut job = job(dir.path(), &[("../a.rs", None), ("../big.txt", None)]);
        job.embed_saved = false;
        job.buffers = vec![(path, true, Rope::from_str("changed\n"))];
        let read = read(&job);
        assert_eq!(read.blocks[0]["resource"]["text"], "changed\n");
        assert_eq!(read.blocks[1]["type"], "resource_link");
    }

    #[test]
    fn what_goes_as_text_has_a_budget() {
        let dir = tempfile::tempdir().unwrap();
        files(dir.path());
        let names: Vec<String> = (0..6).map(|n| format!("f{n}.txt")).collect();
        for name in &names {
            fs::write(dir.path().join("project").join(name), "y".repeat(MOST_FILE)).unwrap();
        }
        let mut job = job(dir.path(), &[]);
        job.mentions = (names.iter())
            .map(|name| (format!("../{name}"), None))
            .collect();
        let read = read(&job);
        let embedded = (read.blocks.iter())
            .filter(|block| block["type"] == "resource")
            .count();
        assert_eq!(embedded, MOST_EMBEDDED / MOST_FILE);
        assert_eq!(read.blocks.len(), 6);
    }

    #[test]
    fn only_a_plain_file_under_the_root_is_read_and_no_more_of_it_than_asked() {
        let dir = tempfile::tempdir().unwrap();
        files(dir.path());
        let root = dir.path().join("project").canonicalize().unwrap();
        // As if swapped in after the path was checked: a link out to an
        // endless device, a pipe no one writes, and a link out of the root.
        std::os::unix::fs::symlink("/dev/zero", root.join("zero")).unwrap();
        let fifo = root.join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .unwrap();
        assert_eq!(read_under(&root.join("zero"), &root, 16), None);
        assert_eq!(read_under(&fifo, &root, 16), None);
        assert_eq!(read_under(&root.join("link.rs"), &root, 16), None);
        assert_eq!(read_under(&root.join("a.rs"), &root, 4), None, "too big");
        assert_eq!(
            read_under(&root.join("a.rs"), &root, 64).as_deref(),
            Some(&b"one\ntwo\nthree\n"[..])
        );
        assert_eq!(
            read_under(&root.join("a.rs"), &dir.path().join("elsewhere"), 64),
            None
        );
    }

    #[test]
    fn lines_past_the_end_are_left_out() {
        assert_eq!(lines_of("a\nb\nc", 2, 9).as_deref(), Some("b\nc"));
        assert_eq!(lines_of("a\nb\n", 3, 4), None);
        assert_eq!(lines_of("a\n", 0, 1), None);
    }

    mod waiting {
        use super::*;
        use crate::chat::Prompt;
        use crate::server::agent_files::tests::daemon_with_chat;

        /// A read that never ends, as on a file system that hangs.
        fn hangs(_: &Job) -> Read {
            loop {
                std::thread::park();
            }
        }

        fn held(daemon: &Daemon, chat: TerminalId) -> bool {
            daemon.mentioning.held.contains_key(&chat)
        }

        /// A daemon with a chat whose message, which mentions a file,
        /// waits for a read that hangs, with a set later held behind it.
        fn hanging(dir: &Path) -> (Daemon, TerminalId) {
            let (mut daemon, chat) = daemon_with_chat(dir);
            daemon.mentioning.reader = hangs;
            daemon.act(chat, vec![Action::Prompt(Prompt::from("look at @a.rs"))]);
            assert!(held(&daemon, chat), "the message waits for its read");
            let set = Action::Set {
                setting: "model".into(),
                value: "opus".into(),
            };
            daemon.act(chat, vec![set]);
            assert_eq!(daemon.mentioning.held[&chat].after.len(), 1);
            (daemon, chat)
        }

        #[test]
        fn a_read_that_hangs_is_given_up_on_and_the_message_goes_without_it() {
            let dir = tempfile::tempdir().unwrap();
            let (mut daemon, chat) = hanging(dir.path());
            assert!(daemon.next_mentions_due().is_some());
            daemon.take_mentions();
            assert!(held(&daemon, chat), "not yet due");
            for reading in &mut daemon.mentioning.reading {
                reading.due = Instant::now();
            }
            daemon.take_mentions();
            assert!(!held(&daemon, chat), "let go");
            assert!(daemon.mentioning.reading.is_empty());
            let notice = daemon.chat(chat).unwrap().node().notice.clone();
            assert!(notice.unwrap().contains("couldn't read @a.rs in time"));
            // The stuck thread doesn't hold the next read up.
            assert_eq!(daemon.mentioning.alive.load(Ordering::Acquire), 1);
            daemon.mentioning.reader = read;
            daemon.act(chat, vec![Action::Prompt(Prompt::from("and @b.rs"))]);
            assert!(held(&daemon, chat));
            let deadline = Instant::now() + Duration::from_secs(10);
            while daemon.mentioning.done.lock().unwrap().is_empty() {
                assert!(Instant::now() < deadline, "the next read never ended");
                std::thread::sleep(Duration::from_millis(5));
            }
            daemon.take_mentions();
            assert!(!held(&daemon, chat));
        }

        #[test]
        fn stop_while_a_read_hangs_acts_at_once_and_drops_the_message() {
            let dir = tempfile::tempdir().unwrap();
            let (mut daemon, chat) = hanging(dir.path());
            daemon.act(chat, vec![Action::Cancel]);
            assert!(!held(&daemon, chat), "the message is dropped");
            assert!(daemon.mentioning.reading.is_empty());
            assert!(daemon.next_mentions_due().is_none());
            // What it read, if it ever ends, is thrown away.
            daemon
                .mentioning
                .done
                .lock()
                .unwrap()
                .push((1, Read::default()));
            daemon.take_mentions();
            assert!(!held(&daemon, chat));
        }
    }
}
