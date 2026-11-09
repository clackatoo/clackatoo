//! The agents' sessions the `:Agent` start page leaves out unless asked
//! (CLACKA-232): the sessions of branches of chats' conversations (see
//! `chat/branches.rs`), which clackatoo forked as a chat was rewound, or
//! left as it switched to another branch. A chat shows its branches
//! inside it, so while it's open its own branches say what to leave out;
//! but neither Claude Code's adapter nor Codex's says what a session was
//! forked from (`forkedFrom`), so once the chat has gone (closed, or not
//! brought back by a fresh start, as chats in no pane aren't) each would
//! be listed as a conversation of its own. The daemon keeps them here,
//! by agent, from one run to the next.
//!
//! A session is kept as it becomes a branch not shown, and let go as it
//! becomes a chat's own again: switched back to, or gone on with from the
//! start page. A chat's own session is never kept, so a chat closed is
//! listed as the branch it showed.
//!
//! They're kept in a file of their own, `unlisted_sessions`, beside the
//! `machine` file in clackatoo's state directory, as `known_machines` is
//! (see `crate::known`), and written as it is: whole, on a thread of its
//! own, for the user alone, the last write always the newest. Read again
//! at every start, upgrades included, so nothing is handed over. JSON, one
//! value: `{"clackatoo_unlisted_sessions": 1, "sessions": [{"agent":
//! "codex", "session": "…"}]}`, the oldest kept first. At most
//! [`MOST_SESSIONS`] are kept: past it, the oldest go, and are listed
//! again. One that can't be read is set aside as
//! `unlisted_sessions.unreadable`, and every session is listed.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// The file's version.
const VERSION: u32 = 1;

/// The most sessions kept, of every agent: far more branches than a person
/// leaves behind in a while.
const MOST_SESSIONS: usize = 1024;

/// The longest agent's name kept, in bytes.
const LONGEST_AGENT: usize = 64;

/// The longest session's ID kept, in bytes, as the longest an agent's
/// listing takes.
const LONGEST_SESSION: usize = 256;

/// The most bytes of the file read: what [`MOST_SESSIONS`] of the longest
/// take, about 400 each, with room to spare.
const MAX_FILE: u64 = 512 * 1024;

/// One session kept, as the file holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Session {
    agent: String,
    session: String,
}

/// The file, as it's written and read.
#[derive(Serialize, Deserialize)]
struct Kept {
    clackatoo_unlisted_sessions: u32,
    #[serde(default)]
    sessions: Vec<Session>,
}

/// The sessions kept off the start page, and where they're kept, if
/// anywhere.
#[derive(Debug, Default)]
pub(crate) struct Unlisted {
    /// The file, or `None` to keep them for this run only, as in tests.
    file: Option<PathBuf>,
    /// The oldest kept first.
    sessions: Arc<Mutex<Vec<Session>>>,
    /// Held while the file is written, so writes never cross.
    writing: Arc<Mutex<()>>,
    /// The threads writing, or about to, so [`kept`](Unlisted::kept) can
    /// wait for them.
    writers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Something kept isn't in the file yet: a change no thread has
    /// written, as none could be started or its write failed.
    unwritten: Arc<AtomicBool>,
}

impl Unlisted {
    /// The sessions `file` keeps, kept there from now on; with no file,
    /// none, kept for this run only.
    pub(crate) fn load(file: Option<PathBuf>) -> Self {
        let sessions = file.as_deref().map(read).unwrap_or_default();
        Self {
            file,
            sessions: Arc::new(Mutex::new(sessions)),
            ..Self::default()
        }
    }

    /// Where they're kept for the daemon whose `machine` file is
    /// `machine`: beside it.
    pub(crate) fn beside(machine: &Path) -> PathBuf {
        machine.with_file_name("unlisted_sessions")
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, Vec<Session>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sessions of the agent called `agent` kept off the start page.
    pub(crate) fn of(&self, agent: &str) -> HashSet<String> {
        (self.sessions().iter())
            .filter(|kept| kept.agent == agent)
            .map(|kept| kept.session.clone())
            .collect()
    }

    /// Keeps `branches`, each a session of the agent it names, off the
    /// start page from now on, and lets `own`, a chat's own session of the
    /// agent it names, be listed again, writing what changed.
    pub(crate) fn note(&self, branches: Vec<(String, String)>, own: Option<(&str, &str)>) {
        let changed = {
            let mut sessions = self.sessions();
            let mut changed = false;
            if let Some((agent, session)) = own {
                let before = sessions.len();
                sessions.retain(|kept| kept.agent != agent || kept.session != session);
                changed = sessions.len() != before;
            }
            for (agent, session) in branches {
                let new = Session { agent, session };
                if !fits(&new) || sessions.contains(&new) {
                    continue;
                }
                if sessions.len() >= MOST_SESSIONS {
                    sessions.remove(0);
                }
                sessions.push(new);
                changed = true;
            }
            changed
        };
        if changed {
            self.keep();
        }
    }

    /// Writes what's kept to the file, on a thread of its own: what's kept
    /// when it writes, so the last to write writes the newest.
    fn keep(&self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        self.unwritten.store(true, Ordering::SeqCst);
        let writing = Arc::clone(&self.writing);
        let sessions = Arc::clone(&self.sessions);
        let unwritten = Arc::clone(&self.unwritten);
        let spawned = std::thread::Builder::new()
            .name("unlisted-sessions".into())
            .spawn(move || {
                let _held = writing.lock().unwrap_or_else(PoisonError::into_inner);
                // Before they're read: a change after it is seen.
                unwritten.store(false, Ordering::SeqCst);
                let sessions = sessions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Err(err) = write(&file, sessions) {
                    unwritten.store(true, Ordering::SeqCst);
                    log::error!("{} couldn't be written: {err}", file.display());
                }
            });
        match spawned {
            Ok(writer) => {
                let mut writers = self.writers.lock().unwrap_or_else(PoisonError::into_inner);
                writers.retain(|writer| !writer.is_finished());
                writers.push(writer);
            }
            Err(err) => log::error!("there's no thread to keep the unlisted sessions on: {err}"),
        }
    }

    /// Waits for what's being written to the file to be written, as the
    /// daemon stops or upgrades: every thread writing, and then once more,
    /// if something kept still isn't in the file.
    pub(crate) fn kept(&self) {
        let writers =
            std::mem::take(&mut *self.writers.lock().unwrap_or_else(PoisonError::into_inner));
        for writer in writers {
            let _ = writer.join();
        }
        if let Some(file) = &self.file
            && self.unwritten.swap(false, Ordering::SeqCst)
        {
            let _held = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
            let sessions = self.sessions().clone();
            if let Err(err) = write(file, sessions) {
                log::error!("{} couldn't be written: {err}", file.display());
            }
        }
    }
}

/// Whether `session` is one to keep: an agent's name and a session's ID
/// no longer than they're ever kept, with no control characters.
fn fits(session: &Session) -> bool {
    let fine = |text: &str, longest: usize| {
        !text.is_empty() && text.len() <= longest && !text.contains(char::is_control)
    };
    fine(&session.agent, LONGEST_AGENT) && fine(&session.session, LONGEST_SESSION)
}

/// The sessions `file` keeps, the oldest first: none if there's no file,
/// and none, with the file set aside, if it can't be read as one. Past
/// [`MOST_SESSIONS`], the oldest are left out.
fn read(file: &Path) -> Vec<Session> {
    let mut bytes = Vec::new();
    match File::open(file) {
        Ok(opened) => {
            if let Err(err) = opened.take(MAX_FILE).read_to_end(&mut bytes) {
                log::error!("{} couldn't be read: {err}", file.display());
                return Vec::new();
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            log::error!("{} couldn't be read: {err}", file.display());
            return Vec::new();
        }
    }
    let kept = match serde_json::from_slice::<Kept>(&bytes) {
        Ok(kept) if kept.clackatoo_unlisted_sessions != 0 => kept,
        Ok(_) | Err(_) => {
            log::error!(
                "{} can't be read: it's set aside, and the start page lists every session",
                file.display()
            );
            let mut aside = file.as_os_str().to_owned();
            aside.push(".unreadable");
            let _ = fs::rename(file, aside);
            return Vec::new();
        }
    };
    let mut sessions: Vec<Session> = Vec::new();
    for session in kept.sessions {
        if fits(&session) && !sessions.contains(&session) {
            sessions.push(session);
        }
    }
    let over = sessions.len().saturating_sub(MOST_SESSIONS);
    if over > 0 {
        sessions = sessions.split_off(over);
    }
    sessions
}

/// Writes `sessions` as the file at `path`, as `crate::known` writes its
/// own.
fn write(path: &Path, sessions: Vec<Session>) -> io::Result<()> {
    let kept = Kept {
        clackatoo_unlisted_sessions: VERSION,
        sessions,
    };
    let mut bytes = serde_json::to_vec_pretty(&kept).map_err(io::Error::other)?;
    bytes.push(b'\n');
    crate::known::write_private(path, &bytes)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn branch(agent: &str, session: &str) -> (String, String) {
        (agent.to_owned(), session.to_owned())
    }

    #[test]
    fn sessions_are_kept_by_agent_for_the_user_alone_and_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = Unlisted::beside(&dir.path().join("machine"));
        let unlisted = Unlisted::load(Some(file.clone()));
        unlisted.note(vec![branch("codex", "s1"), branch("claude", "c1")], None);
        unlisted.kept();
        assert_eq!(unlisted.of("codex"), HashSet::from(["s1".to_owned()]));
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let again = Unlisted::load(Some(file.clone()));
        assert_eq!(again.of("codex"), HashSet::from(["s1".to_owned()]));
        assert_eq!(again.of("claude"), HashSet::from(["c1".to_owned()]));
        assert!(again.of("gemini").is_empty());
        // Gone on with, it's a chat's own: listed again, from now on.
        again.note(Vec::new(), Some(("codex", "s1")));
        again.kept();
        assert!(Unlisted::load(Some(file)).of("codex").is_empty());
    }

    #[test]
    fn so_many_are_kept_and_no_more_the_oldest_listed_again() {
        let unlisted = Unlisted::load(None);
        let many = (0..=MOST_SESSIONS).map(|n| branch("codex", &format!("s{n}")));
        unlisted.note(many.collect(), None);
        let kept = unlisted.of("codex");
        assert_eq!(kept.len(), MOST_SESSIONS);
        assert!(!kept.contains("s0"), "the oldest went");
        assert!(kept.contains(&format!("s{MOST_SESSIONS}")));
        // Nothing that doesn't fit, nor the same twice.
        unlisted.note(
            vec![
                branch("codex", "s1"),
                branch("codex", "a\nb"),
                branch("", "s"),
                branch("codex", &"x".repeat(LONGEST_SESSION + 1)),
            ],
            None,
        );
        assert_eq!(unlisted.of("codex").len(), MOST_SESSIONS);
        assert!(unlisted.of("").is_empty());
    }

    #[test]
    fn a_file_that_cant_be_read_is_set_aside_and_every_session_listed() {
        let dir = tempfile::tempdir().unwrap();
        let file = Unlisted::beside(&dir.path().join("machine"));
        fs::write(&file, "not json").unwrap();
        let unlisted = Unlisted::load(Some(file.clone()));
        assert!(unlisted.of("codex").is_empty());
        assert!(!file.exists());
        assert!(dir.path().join("unlisted_sessions.unreadable").exists());
    }
}
