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
//! it: canonicalized, so a link can't lead out) stays words, and the chat
//! says so; one that names nothing is words. A slash command's message is
//! read for none, as the adapters look for the command in its last or
//! only block.
//!
//! The disk is read on a thread of its own, never the event loop: the
//! message, and every action for its chat after it, wait for it (see
//! [`Daemon::act`]), so nothing overtakes it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use clackatoo_protocol::{TerminalId, lines_suffix, mentions, slash_command};
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
const MOST_READ_FOR_LINES: u64 = 8 * 1024 * 1024;

/// What's to be read for a message's mentions, gathered on the loop.
#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) chat: TerminalId,
    /// Each mention: its path as written, and its lines.
    pub(crate) mentions: Vec<(String, Option<(u32, u32)>)>,
    /// The chat's working directory and project.
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

/// The thread that reads mentions: where jobs go to it, and where what
/// it read comes back, by chat.
type Worker = (mpsc::Sender<Job>, mpsc::Receiver<(TerminalId, Read)>);

/// The messages waiting for their mentions to be read, and the thread
/// that reads them.
#[derive(Debug, Default)]
pub(super) struct Mentioning {
    worker: Option<Worker>,
    /// Each chat's actions waiting, from the message being read for.
    held: HashMap<TerminalId, VecDeque<Action>>,
    /// What was read for each chat's message, until it's sent.
    read: HashMap<TerminalId, Vec<Value>>,
}

impl Mentioning {
    /// Reads `job` on the thread, starting it if it isn't going, and
    /// wakes the loop with `waker` once it's read. Returns whether it
    /// will be.
    fn submit(&mut self, job: Job, waker: &Arc<mio::Waker>) -> bool {
        if self.worker.is_none() {
            let (send, jobs) = mpsc::channel::<Job>();
            let (done, receive) = mpsc::channel();
            let waker = Arc::clone(waker);
            let spawned = std::thread::Builder::new()
                .name("mentions".into())
                .spawn(move || {
                    while let Ok(job) = jobs.recv() {
                        let chat = job.chat;
                        if done.send((chat, read(&job))).is_err() {
                            break;
                        }
                        if let Err(err) = waker.wake() {
                            log::warn!("waking the event loop for mentions failed: {err}");
                        }
                    }
                });
            if let Err(err) = spawned {
                log::error!("the mentions thread couldn't start: {err}");
                return false;
            }
            self.worker = Some((send, receive));
        }
        let sent = (self.worker.as_ref()).is_some_and(|(send, _)| send.send(job).is_ok());
        if !sent {
            self.worker = None;
        }
        sent
    }
}

impl Daemon {
    /// Whether `id`'s actions wait for a message's mentions to be read.
    pub(super) fn holding_for_mentions(
        &mut self,
        id: TerminalId,
        actions: &mut Vec<Action>,
    ) -> bool {
        match self.mentioning.held.get_mut(&id) {
            Some(held) => {
                held.extend(actions.drain(..));
                true
            }
            None => false,
        }
    }

    /// The blocks to send after `action`'s text, a message to the chat in
    /// `id`: those read for it, or none. Starts reading them, if they're
    /// to be read and haven't been, and returns `Err` with the action, and
    /// what follows it held until they have.
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
        if let Some(read) = self.mentioning.read.remove(&id) {
            return Ok((action, read));
        }
        let Some(job) = self.mention_job(id, text) else {
            return Ok((action, Vec::new()));
        };
        if !self.mentioning.submit(job, &self.waker) {
            return Ok((action, Vec::new()));
        }
        let held = std::iter::once(action).chain(after.by_ref()).collect();
        self.mentioning.held.insert(id, held);
        Err(())
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
        let cwd = PathBuf::from(chat.working_directory());
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
            chat: id,
            mentions,
            cwd,
            project,
            home,
            buffers,
            embed,
            embed_saved,
        })
    }

    /// Sends the messages whose mentions have been read, and what was held
    /// behind them.
    pub(super) fn take_mentions(&mut self) {
        let Some((_, receive)) = &self.mentioning.worker else {
            return;
        };
        let done: Vec<(TerminalId, Read)> = receive.try_iter().collect();
        for (id, read) in done {
            let Some(held) = self.mentioning.held.remove(&id) else {
                continue;
            };
            if !read.outside.is_empty()
                && let Some(chat) = self.chat(id)
            {
                let named = read.outside.join(", ");
                chat.notify(format!(
                    "{named} isn't in the chat's project, so the agent was sent the words, not the file"
                ));
                self.collect_damage(id);
            }
            self.mentioning.read.insert(id, read.blocks);
            self.act(id, held.into());
            // Read for a message that wasn't sent after all.
            self.mentioning.read.remove(&id);
        }
    }
}

/// Where `path`, mentioned in a chat working in `cwd` in `project`, may
/// be, as written, in the order looked: from the working directory, as
/// the agent reads it, then from the project's root, as the window's
/// completion offers it.
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
    let Ok(root) = job.project.canonicalize() else {
        return made;
    };
    let mut budget = MOST_EMBEDDED;
    let mut seen = HashSet::new();
    for (written, lines) in &job.mentions {
        let candidates = places(written, &job.cwd, &job.project, &job.home);
        let Some(real) = candidates
            .iter()
            .find_map(|place| place.canonicalize().ok())
        else {
            continue;
        };
        if !real.starts_with(&root) {
            let shown = clackatoo_protocol::mention(written, *lines);
            if !made.outside.contains(&shown) {
                made.outside.push(shown);
            }
            continue;
        }
        if !seen.insert((real.clone(), *lines)) {
            continue;
        }
        let Ok(meta) = fs::metadata(&real) else {
            continue;
        };
        let Ok(mut uri) = url::Url::from_file_path(&real) else {
            continue;
        };
        let suffix = lines_suffix(*lines);
        if let Some(fragment) = suffix.strip_prefix('#') {
            uri.set_fragment(Some(fragment));
        }
        let buffer =
            (job.buffers.iter()).find(|(path, ..)| *path == real || candidates.contains(path));
        let text = if !job.embed || !meta.is_file() {
            None
        } else if let Some((first, last)) = *lines {
            let whole = match buffer {
                Some((_, _, rope)) => Some(rope.to_string()),
                None if meta.len() <= MOST_READ_FOR_LINES => fs::read_to_string(&real).ok(),
                None => None,
            };
            whole.and_then(|whole| lines_of(&whole, first, last))
        } else if let Some((_, true, rope)) = buffer {
            (rope.len_bytes() <= MOST_FILE).then(|| rope.to_string())
        } else if job.embed_saved && meta.len() <= MOST_FILE as u64 {
            read_small(&real)
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

/// The file at `path`, if it's text and no more than [`MOST_FILE`].
fn read_small(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    let file = fs::File::open(path).ok()?;
    file.take(MOST_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MOST_FILE {
        return None;
    }
    String::from_utf8(bytes).ok()
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
        Job {
            chat: TerminalId(1),
            mentions: (mentions.iter())
                .map(|&(path, lines)| (path.to_owned(), lines))
                .collect(),
            cwd: dir.join("project/sub"),
            project: dir.join("project"),
            home: dir.to_owned(),
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
        assert_eq!(read.outside, ["@~/outside.rs", "@link.rs"]);
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
        let mentions: Vec<(String, Option<(u32, u32)>)> = (names.iter())
            .map(|name| (format!("../{name}"), None))
            .collect();
        let mut job = job(dir.path(), &[]);
        job.mentions = mentions;
        let read = read(&job);
        let embedded = (read.blocks.iter())
            .filter(|block| block["type"] == "resource")
            .count();
        assert_eq!(embedded, MOST_EMBEDDED / MOST_FILE);
        assert_eq!(read.blocks.len(), 6);
    }

    #[test]
    fn lines_past_the_end_are_left_out() {
        assert_eq!(lines_of("a\nb\nc", 2, 9).as_deref(), Some("b\nc"));
        assert_eq!(lines_of("a\nb\n", 3, 4), None);
        assert_eq!(lines_of("a\n", 0, 1), None);
    }
}
