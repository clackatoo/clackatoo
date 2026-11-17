//! The catalogues of shared skills chats have, read off the event loop.
//!
//! A catalogue is read for a chat's [`Roots`] on a worker, and kept: every
//! chat with the same roots shares it, and a read made a moment ago isn't
//! made again. A chat's agent connects once its catalogue is there, or,
//! if reading takes too long, without one, and asks again the next time
//! it connects. What a read finds in the user's own folders is every
//! chat's, so a change there has the catalogues of the other folders with
//! chats in them read again too. A skill's instructions are read from its
//! own file as they're asked for, on a worker: nothing else is read then.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Error, PaneId, Response, TerminalId, WorkState};
use mio::Token;
use serde_json::Value;

use super::{Daemon, Source};
use crate::skills::{Catalogue, Roots, Unloadable};
use crate::work::{Priority, Step};

/// How long a read may take before what waits on it goes on without.
const DEADLINE: Duration = Duration::from_secs(3);

/// The most reads waiting or going at once, each shared by whatever asks
/// for the same catalogue: half the room the background work has, so
/// reading skills never keeps other work from starting.
const MOST_READING: usize = 32;

/// The most askers waiting on reads at once, in all.
const MOST_WAITING: usize = 1024;

/// How long a catalogue just read is taken as what's on disk: the chats
/// an agent starts together, and a window's requests as Settings opens,
/// share one read.
const FRESH: Duration = Duration::from_secs(2);

/// How long the project found for a folder that isn't a chat's is taken
/// as its project, for Settings, which asks every few seconds.
const PROJECT_KNOWN_FOR: Duration = Duration::from_secs(30);

/// How many times reading a catalogue again is tried, after what decides
/// it changed, before the chats that had it are left without.
const MOST_RETRIES: u8 = 3;

/// The most folders whose projects are kept.
const MOST_LOOKED: usize = 8;

/// What an agent is told of a call whose answer would be from before
/// what it's offered changed.
const CHANGED: &str = "The skills offered here changed while this was read. Ask again.";

/// The most catalogues kept, the least lately read let go first.
const MOST_KEPT: usize = 32;

/// Why a catalogue, or a skill, couldn't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Failed {
    /// Too much is being read already.
    Busy,
    /// It took longer than [`DEADLINE`].
    TimedOut,
    /// The user cancelled the read, in the list of background work.
    Cancelled,
    /// The worker reading it stopped.
    Stopped,
    /// The daemon is upgrading, which ends its workers.
    Upgrading,
    /// The read couldn't be started, as the scheduler says.
    Unstarted(String),
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("Too many skill requests. Try again shortly."),
            Self::TimedOut => {
                f.write_str("Reading skill folders took too long. Try again shortly.")
            }
            Self::Cancelled => f.write_str("Reading skills was cancelled. Try again."),
            Self::Stopped => f.write_str("The worker reading skills stopped. Try again."),
            Self::Upgrading => {
                f.write_str("The daemon is upgrading. Retry the skill request after it reconnects.")
            }
            Self::Unstarted(why) => f.write_str(why),
        }
    }
}

/// An agent's call to a skill tool, waiting for its answer.
#[derive(Debug)]
struct Asker {
    token: Token,
    id: Value,
    chat: TerminalId,
    /// The connection's secret, as a token is used again once its
    /// connection has gone: the answer goes only to the one that asked.
    secret: Option<String>,
}

/// What a read is for.
#[derive(Debug)]
enum Purpose {
    /// The chat's agent connects once it's read.
    Connect(TerminalId),
    /// A chat is to have the catalogue as it is now, without connecting
    /// again: this one, or with `None`, every chat with these roots.
    Refresh(Option<TerminalId>),
    /// Settings lists the skills.
    Browse { token: Token, ticket: u64 },
    /// Settings lists the specialists, with their files if `files`.
    BrowseSpecialists {
        token: Token,
        ticket: u64,
        files: bool,
    },
    /// An agent's `list_skills`.
    List(Asker),
    /// An agent's `load_skill` of the skill `name`, in a chat that has no
    /// catalogue yet: the skill is read once the catalogue is.
    Load {
        asker: Asker,
        name: String,
        arguments: String,
    },
    /// The skill an agent's `load_skill` asked for is being read.
    Loading(Asker),
}

impl Purpose {
    /// The chat it's for, if it's for one.
    fn chat(&self) -> Option<TerminalId> {
        match self {
            Self::Connect(chat) | Self::Refresh(Some(chat)) => Some(*chat),
            Self::List(asker) | Self::Load { asker, .. } | Self::Loading(asker) => Some(asker.chat),
            Self::Refresh(None) | Self::Browse { .. } | Self::BrowseSpecialists { .. } => None,
        }
    }
}

/// Something waiting on a read.
#[derive(Debug)]
struct Pending {
    /// The read it waits on, by the number of the request that started
    /// it: its own, unless it shares another's.
    reading: u64,
    /// That read's job, in the background work.
    work: u64,
    /// The [`Skills::epoch`] it was asked in.
    epoch: u64,
    purpose: Purpose,
    roots: Roots,
    due: Instant,
}

/// What a read came to.
#[derive(Debug)]
enum Done {
    Catalogue(Arc<Catalogue>),
    Skill(Result<String, Unloadable>),
}

/// A catalogue kept, for whatever asks for the same roots.
#[derive(Debug)]
struct Kept {
    roots: Roots,
    catalogue: Arc<Catalogue>,
    read: Instant,
}

/// A chat's catalogue, and the roots it was read for: when the chat's
/// roots are no longer those, as its project is trusted or not, or a
/// skill is switched off, it's no longer the chat's.
#[derive(Debug)]
struct Held {
    roots: Roots,
    catalogue: Arc<Catalogue>,
}

/// A catalogue to be read again, as reading it failed after what decides
/// it changed.
#[derive(Debug)]
struct Retry {
    roots: Roots,
    /// How many reads have failed.
    tries: u8,
    /// Whether no read of it is under way, so one is to be started.
    waiting: bool,
}

/// The project found for a folder that isn't a chat's.
#[derive(Debug)]
struct Looked {
    /// The folder, as it was asked about.
    cwd: PathBuf,
    /// The folder with links followed, and the project it's in.
    real: PathBuf,
    project: PathBuf,
    at: Instant,
}

/// The catalogues the daemon has, and the reads under way.
#[derive(Debug, Default)]
pub(super) struct Skills {
    /// Counts what makes every read under way out of date: a specialist
    /// or an MCP server saved from Settings.
    epoch: u64,
    /// Numbers the requests.
    next: u64,
    pending: BTreeMap<u64, Pending>,
    /// What the workers have read, by the read's number, for
    /// [`Daemon::take_skills`].
    done: Arc<Mutex<Vec<(u64, Done)>>>,
    kept: Vec<Kept>,
    /// Each chat's catalogue: the one kept for its roots, or, for a chat
    /// started as a specialist, what the specialist chooses of it.
    catalogues: BTreeMap<TerminalId, Held>,
    /// The catalogues to read again, as reading them failed.
    retries: Vec<Retry>,
    /// The projects lately found for folders that aren't chats', one for
    /// each window with Settings open somewhere else.
    looked: Mutex<Vec<Looked>>,
    /// How many times a chat has been given a catalogue: what a read
    /// costs the event loop, for tests to count.
    #[cfg(test)]
    given: u64,
    /// The session and the context each chat's agent was last told its
    /// catalogue in.
    announced: BTreeMap<TerminalId, (String, u64)>,
    /// The chats connecting without a catalogue, as theirs couldn't be
    /// read: only while each connects, so the next time it does, it's
    /// read again.
    skipped: BTreeSet<TerminalId>,
    /// The home directory with links followed, worked out once.
    home: std::sync::OnceLock<PathBuf>,
}

impl Skills {
    /// The catalogue kept for `roots`, if one is.
    fn kept(&self, roots: &Roots) -> Option<&Kept> {
        self.kept.iter().find(|kept| kept.roots == *roots)
    }

    /// Keeps `catalogue`, just read for `roots`.
    fn keep(&mut self, roots: &Roots, catalogue: &Arc<Catalogue>) {
        self.kept.retain(|kept| kept.roots != *roots);
        if self.kept.len() >= MOST_KEPT {
            let oldest = (self.kept.iter().enumerate())
                .min_by_key(|(_, kept)| kept.read)
                .map(|(at, _)| at);
            if let Some(at) = oldest {
                self.kept.swap_remove(at);
            }
        }
        self.kept.push(Kept {
            roots: roots.clone(),
            catalogue: Arc::clone(catalogue),
            read: Instant::now(),
        });
    }
}

/// Whether `work`, a read's job in `scheduler`, gives a result: it's
/// waiting, being read, or read, and not cancelled or failed.
fn will_be_read(scheduler: &crate::work::Scheduler, work: u64) -> bool {
    matches!(
        scheduler.state(work),
        Some(WorkState::Queued | WorkState::Running | WorkState::Completed)
    )
}

impl Daemon {
    /// The chat `chat`'s catalogue, once it's been read.
    pub(super) fn skill_catalogue(&self, chat: TerminalId) -> Option<&Catalogue> {
        let held = self.skills.catalogues.get(&chat)?;
        Some(&held.catalogue)
    }

    /// The chat's catalogue, to read a skill from on a worker.
    pub(super) fn shared_skill_catalogue(&self, chat: TerminalId) -> Option<Arc<Catalogue>> {
        let held = self.skills.catalogues.get(&chat)?;
        Some(Arc::clone(&held.catalogue))
    }

    /// Lets go of the chat `chat`'s catalogue: it's closing, or becoming
    /// another specialist, and its next connection reads one again.
    pub(super) fn forget_skill_catalogue(&mut self, chat: TerminalId) {
        self.skills.catalogues.remove(&chat);
        self.skills.announced.remove(&chat);
        self.skills.skipped.remove(&chat);
    }

    /// The roots of a catalogue for the folder `cwd`, which isn't known
    /// to be a chat's: Settings', for the pane it was opened from. A chat
    /// there says what its project is. With none, it's looked for on
    /// disk, as it is when a chat is added, and kept a while: Settings
    /// asks every few seconds while it's open.
    pub(super) fn skill_roots(&self, cwd: PathBuf) -> Roots {
        self.folder_roots(cwd, false)
    }

    /// As [`skill_roots`](Self::skill_roots), with the project looked for
    /// afresh: for a save, which writes into it, so a repository made or
    /// removed a moment ago is where it's found to be now.
    pub(super) fn skill_roots_now(&self, cwd: PathBuf) -> Roots {
        self.folder_roots(cwd, true)
    }

    /// The roots of a catalogue for the folder `cwd`: with its project
    /// looked for on disk if `afresh`, or if it isn't lately known.
    fn folder_roots(&self, cwd: PathBuf, afresh: bool) -> Roots {
        let chats = || (self.terminals.values()).filter_map(|slot| slot.terminal.chat_ref());
        if let Some(chat) = chats().find(|chat| chat.real_cwd() == cwd) {
            let project = chat.project().to_owned();
            return self.roots_of(cwd, project);
        }
        let mut looked = (self.skills.looked.lock()).unwrap_or_else(PoisonError::into_inner);
        looked.retain(|looked| looked.at.elapsed() < PROJECT_KNOWN_FOR);
        let known = (looked.iter())
            .find(|looked| looked.cwd == cwd)
            .filter(|_| !afresh)
            .map(|looked| (looked.real.clone(), looked.project.clone()));
        let (real, project) = known.unwrap_or_else(|| {
            // With links followed, as a chat's is, so the roots are those
            // of a chat there.
            let real = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
            let project =
                clackatoo_syntax::git_root(&real, self.real_home()).unwrap_or(real.clone());
            looked.retain(|looked| looked.cwd != cwd);
            if looked.len() >= MOST_LOOKED {
                looked.remove(0);
            }
            looked.push(Looked {
                cwd,
                real: real.clone(),
                project: project.clone(),
                at: Instant::now(),
            });
            (real, project)
        });
        drop(looked);
        // A chat there, reached by another name for the folder.
        let project = chats()
            .find(|chat| chat.real_cwd() == real)
            .map_or(project, |chat| chat.project().to_owned());
        self.roots_of(real, project)
    }

    /// The roots of the chat `chat`'s catalogue.
    fn chat_roots(&self, chat: TerminalId) -> Option<Roots> {
        let own = self.chat_ref(chat)?;
        let (cwd, project) = (own.real_cwd(), own.project().to_owned());
        Some(self.roots_of(cwd, project))
    }

    /// The home directory with links followed, worked out once, as it
    /// reads the disk. A chat's working directory and project have their
    /// links followed too, so this is the home they're found to be in or
    /// under: where `/home` is a link, the one in the environment isn't.
    /// Only what was found is kept: while home can't be read, as before
    /// it's mounted, it's the one in the environment, and asked again.
    pub(super) fn real_home(&self) -> &std::path::Path {
        if let Some(home) = self.skills.home.get() {
            return home;
        }
        match self.config.home.canonicalize() {
            Ok(real) => self.skills.home.get_or_init(|| real),
            Err(_) => &self.config.home,
        }
    }

    /// The roots of a catalogue for `cwd`, in `project`: with what the
    /// user has said of that project, and of skills, as it is now.
    fn roots_of(&self, cwd: PathBuf, project: PathBuf) -> Roots {
        let home = self.real_home().to_owned();
        let config = (self.config.own_config.as_ref())
            .and_then(|own| own.dirs.daemon.parent().map(std::path::Path::to_owned));
        let roots = Roots {
            cwd,
            project,
            home,
            config,
            trusted: false,
            disabled: Arc::clone(&self.config.agents.configured.disabled_skills),
        };
        // Each folder looked in must be trusted: one marked not trusted
        // inside a project that is stays unread.
        let trusted = roots.project_folders().all(|folder| self.trusts(folder));
        Roots { trusted, ..roots }
    }

    /// Reads the catalogue for `roots`, for `purpose`, which
    /// [`skills_finished`](Self::skills_finished) is called with once it's
    /// read, or can't be. One read a moment ago is used as it is, and a
    /// read under way for the same roots is shared.
    fn read_catalogue(&mut self, purpose: Purpose, roots: Roots) {
        let recent = (self.skills.kept(&roots))
            .filter(|kept| kept.read.elapsed() < FRESH)
            .map(|kept| Arc::clone(&kept.catalogue));
        if let Some(catalogue) = recent {
            // The chats there have it already, but one that asks now.
            let blocked = match purpose {
                Purpose::Connect(chat) | Purpose::Refresh(Some(chat)) => {
                    self.give_catalogue(&roots, &catalogue, Some(chat), &purpose)
                }
                Purpose::Refresh(None) => self.give_catalogue(&roots, &catalogue, None, &purpose),
                _ => BTreeSet::new(),
            };
            self.skills_finished(purpose, &roots, Ok(catalogue), &blocked);
            return;
        }
        // A read of the same catalogue whose result hasn't been taken
        // yet, whether or not its worker has finished.
        let shared = (self.skills.pending.values())
            .find(|pending| {
                pending.epoch == self.skills.epoch
                    && pending.roots == roots
                    && !matches!(pending.purpose, Purpose::Loading(_))
                    && will_be_read(&self.work.scheduler, pending.work)
            })
            .map(|pending| (pending.reading, pending.work));
        // Counted after what's shared, so any number of chats in one
        // folder, as when the daemon restarts with many, wait on one read.
        let reads: BTreeSet<u64> = (self.skills.pending.values())
            .map(|pending| pending.reading)
            .collect();
        let full = shared.is_none() && reads.len() >= MOST_READING;
        if full || self.skills.pending.len() >= MOST_WAITING {
            self.skills_failed(purpose, &roots, Failed::Busy);
            return;
        }
        self.skills.next += 1;
        let job = self.skills.next;
        let due = Instant::now() + DEADLINE;
        let epoch = self.skills.epoch;
        // Already read, and waiting to be taken: nothing more to start.
        if let Some((reading, work)) = shared
            && self.work.scheduler.state(work) == Some(WorkState::Completed)
        {
            let pending = Pending {
                reading,
                work,
                epoch,
                purpose,
                roots,
                due,
            };
            self.skills.pending.insert(job, pending);
            return;
        }
        let reading = shared.map_or(job, |(reading, _)| reading);
        let priority = if matches!(purpose, Purpose::Refresh(_)) {
            Priority::Background
        } else {
            Priority::Interactive
        };
        let done = Arc::clone(&self.skills.done);
        let read = roots.clone();
        // The key of the read it shares, which only raises that read's
        // priority, or one of its own: a worker still returning from a
        // read whose result was taken already is never given another.
        let key = format!("skills:{}:{reading}", self.skills.epoch);
        let submitted = self.work.scheduler.submit(
            key,
            "Reading skills",
            Some(roots.cwd.to_string_lossy().into_owned()),
            priority,
            move |context| {
                if context.cancelled() {
                    return Ok(Step::Done);
                }
                context.progress("Discovering skills and specialists", 0, None);
                let catalogue = Arc::new(Catalogue::discover(&read));
                if !context.cancelled() {
                    done.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((reading, Done::Catalogue(catalogue)));
                }
                Ok(Step::Done)
            },
        );
        match submitted {
            Ok(work) => {
                let pending = Pending {
                    reading,
                    work,
                    epoch,
                    purpose,
                    roots,
                    due,
                };
                self.skills.pending.insert(job, pending);
            }
            Err(why) => self.skills_failed(purpose, &roots, Failed::Unstarted(why)),
        }
    }

    /// Reads the skill `name` of `catalogue` for `asker`, an agent's
    /// `load_skill`, on a worker: its file only.
    fn load_skill(
        &mut self,
        asker: Asker,
        catalogue: Arc<Catalogue>,
        name: String,
        arguments: String,
    ) {
        if self.skills.pending.len() >= MOST_WAITING {
            self.answer_skill_tool(&asker, Err(Failed::Busy.to_string()));
            return;
        }
        let Some(roots) = self.chat_roots(asker.chat) else {
            self.answer_skill_tool(&asker, Err("The chat has closed.".into()));
            return;
        };
        self.skills.next += 1;
        let job = self.skills.next;
        let done = Arc::clone(&self.skills.done);
        let submitted = self.work.scheduler.submit(
            format!("skill:{job}"),
            "Reading a skill",
            Some(name.clone()),
            Priority::Interactive,
            move |context| {
                if !context.cancelled() {
                    let loaded = catalogue.load(&name, &arguments, false);
                    done.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((job, Done::Skill(loaded)));
                }
                Ok(Step::Done)
            },
        );
        match submitted {
            Ok(work) => {
                let pending = Pending {
                    reading: job,
                    work,
                    epoch: self.skills.epoch,
                    purpose: Purpose::Loading(asker),
                    roots,
                    due: Instant::now() + DEADLINE,
                };
                self.skills.pending.insert(job, pending);
            }
            Err(why) => self.answer_skill_tool(&asker, Err(why)),
        }
    }

    /// Holds a new connection while its catalogue is read: returns whether
    /// the chat `chat` waits, to connect once it has one.
    pub(super) fn reading_skills(&mut self, chat: TerminalId) -> bool {
        let side = (self.chat_ref(chat)).is_some_and(|own| own.side_parent().is_some());
        if side || self.skills.catalogues.contains_key(&chat) {
            return false;
        }
        // Its read failed: it's connecting without (see
        // `connect_without_skills`).
        if self.skills.skipped.contains(&chat) {
            return false;
        }
        let waiting = (self.skills.pending.values())
            .any(|pending| matches!(pending.purpose, Purpose::Connect(id) if id == chat));
        if !waiting {
            let Some(roots) = self.chat_roots(chat) else {
                return false;
            };
            self.read_catalogue(Purpose::Connect(chat), roots);
        }
        true
    }

    /// A kept runtime needs its host catalogue back without opening its
    /// already-live session again.
    pub(super) fn refresh_chat_skills(&mut self, chat: TerminalId) {
        if let Some(roots) = self.chat_roots(chat) {
            self.read_catalogue(Purpose::Refresh(Some(chat)), roots);
        }
    }

    /// Reads every chat's catalogue again, but those with `but` for
    /// roots: what decides them changed, or what the user's own folders
    /// hold did.
    fn refresh_skills_but(&mut self, but: Option<&Roots>) {
        let chats: Vec<_> = (self.terminals.iter())
            .filter(|(_, slot)| slot.terminal.chat_ref().is_some())
            .map(|(&id, _)| id)
            .collect();
        let mut asked = Vec::new();
        for chat in chats {
            let Some(roots) = self.chat_roots(chat) else {
                continue;
            };
            if Some(&roots) != but && !asked.contains(&roots) {
                asked.push(roots.clone());
                self.read_catalogue(Purpose::Refresh(None), roots);
            }
        }
    }

    /// What's on disk was changed from here, or what the user trusts or
    /// has switched off was: nothing kept or being read is current, and
    /// every chat's catalogue is read again.
    ///
    /// A chat whose roots are no longer those its catalogue was read for
    /// loses it at once, before anything is read: a folder no longer
    /// trusted, or a skill switched off, stops being offered whether or
    /// not the read that follows succeeds. One that fails is tried again.
    pub(super) fn skills_changed(&mut self) {
        self.skills.epoch += 1;
        self.skills.kept.clear();
        let held: Vec<_> = self.skills.catalogues.keys().copied().collect();
        for chat in held {
            let now = self.chat_roots(chat);
            let was = self.skills.catalogues.get(&chat).map(|held| &held.roots);
            if now.as_ref() == was {
                continue;
            }
            self.skills.catalogues.remove(&chat);
            // Once for its roots, however many chats have them.
            let retries = &mut self.skills.retries;
            let again = now.filter(|roots| !retries.iter().any(|retry| retry.roots == *roots));
            retries.extend(again.map(|roots| Retry {
                roots,
                tries: 0,
                waiting: false,
            }));
            let specialists = self.specialists_for(chat);
            if let Some(own) = self.chat(chat) {
                own.offer_skills(Vec::new());
                own.offer_specialists(&specialists);
            }
            self.collect_damage(chat);
        }
        self.refresh_skills_but(None);
    }

    /// A read of the catalogue for `roots`, to have chats there offered
    /// it as it is now, failed, as `failed` says. If it's one that's
    /// being tried again, it waits to be: but not for ever.
    fn refresh_failed(&mut self, roots: &Roots, failed: &Failed) {
        log::warn!("refreshing shared skills: {failed}");
        let Some(at) = (self.skills.retries.iter()).position(|retry| retry.roots == *roots) else {
            return;
        };
        let retry = &mut self.skills.retries[at];
        // No room to read is no try at reading.
        if *failed != Failed::Busy {
            retry.tries += 1;
        }
        retry.waiting = true;
        if retry.tries >= MOST_RETRIES {
            log::warn!(
                "shared skills: {} is left unread after {MOST_RETRIES} tries",
                roots.cwd.display()
            );
            self.skills.retries.swap_remove(at);
        }
    }

    /// Starts again the reads that failed, where there's room, and lets
    /// go of those whose chats have gone or have moved on.
    fn retry_refreshes(&mut self) {
        let chats: Vec<_> = self.terminals.keys().copied().collect();
        let wanted: Vec<Roots> = (chats.into_iter())
            .filter(|chat| !self.skills.catalogues.contains_key(chat))
            .filter_map(|chat| self.chat_roots(chat))
            .collect();
        (self.skills.retries).retain(|retry| wanted.contains(&retry.roots));
        let reads: BTreeSet<u64> = (self.skills.pending.values())
            .map(|pending| pending.reading)
            .collect();
        let mut room = MOST_READING.saturating_sub(reads.len());
        let mut again = Vec::new();
        for retry in &mut self.skills.retries {
            if retry.waiting && room > 0 {
                retry.waiting = false;
                room -= 1;
                again.push(retry.roots.clone());
            }
        }
        for roots in again {
            self.read_catalogue(Purpose::Refresh(None), roots);
        }
    }

    /// Lists the skills for the window at `token`, as they are for what
    /// `pane` shows.
    pub(super) fn browse_skills(&mut self, token: Token, pane: Option<PaneId>) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        if let Err(err) = self.refuse_lent(token, &clackatoo_protocol::Request::Skills { pane }) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return;
        }
        if self.is_web_device(token) {
            let refusal = "Browse shared skills in a clackatoo window.".to_owned();
            self.hand_answer_with(token, ticket, Response::Error(Error::Other(refusal)));
            return;
        }
        let roots = self.pane_roots(pane);
        self.read_catalogue(Purpose::Browse { token, ticket }, roots);
    }

    /// Lists the specialists for the window at `token`.
    pub(super) fn browse_specialists(&mut self, token: Token, pane: Option<PaneId>) {
        self.browse_specialists_as(token, pane, false);
    }

    /// Lists the specialists for the window at `token`, with their files.
    pub(super) fn browse_specialist_files(&mut self, token: Token, pane: Option<PaneId>) {
        self.browse_specialists_as(token, pane, true);
    }

    /// Lists the specialists for the window at `token`, as they are for
    /// what `pane` shows: with their files, if `files`.
    fn browse_specialists_as(&mut self, token: Token, pane: Option<PaneId>, files: bool) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        if let Some(err) = self.not_a_window(token) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return;
        }
        let roots = self.pane_roots(pane);
        let purpose = Purpose::BrowseSpecialists {
            token,
            ticket,
            files,
        };
        self.read_catalogue(purpose, roots);
    }

    /// The roots of the catalogue for what `pane` shows: its program's
    /// working directory, or the home directory.
    fn pane_roots(&self, pane: Option<PaneId>) -> Roots {
        let cwd = pane
            .and_then(|pane| self.working_directory(pane))
            .map_or_else(|| self.config.home.clone(), PathBuf::from);
        self.skill_roots(cwd)
    }

    /// Takes `catalogue`, read for `roots` by whatever saved an MCP
    /// server from Settings, or looked at them: kept, and given to the
    /// chats there if it offers anything it didn't. Settings asks every
    /// few seconds while it's open, so one that changes nothing costs a
    /// comparison, and nothing for each chat.
    pub(super) fn refresh_mcp_catalogue(&mut self, roots: &Roots, catalogue: Catalogue) {
        let catalogue = Arc::new(catalogue);
        let old = self.skills.kept(roots).map(|kept| &kept.catalogue);
        let servers_changed = old.is_none_or(|old| old.managed_mcp != catalogue.managed_mcp);
        let same = old.is_some_and(|old| old.same_offer(&catalogue));
        if servers_changed {
            // What's being read was read with the servers as they were.
            self.skills.epoch += 1;
        }
        self.skills.keep(roots, &catalogue);
        if !same {
            self.give_catalogue(roots, &catalogue, None, &Purpose::Refresh(None));
        }
    }

    /// An agent's `list_skills`, or its `load_skill` of the skill and
    /// arguments `load`, for the chat `chat`.
    pub(super) fn skill_tool(
        &mut self,
        token: Token,
        id: Value,
        chat: TerminalId,
        load: Option<(String, String)>,
    ) {
        let Some(roots) = self.chat_roots(chat) else {
            self.answer_tool(token, &id, Err("The chat has closed.".into()));
            return;
        };
        let secret = match self.sources.get(token.0) {
            Some(Source::Tools(connection)) if connection.chat == chat => connection.token.clone(),
            _ => return,
        };
        let asker = Asker {
            token,
            id,
            chat,
            secret,
        };
        let Some((name, arguments)) = load else {
            self.read_catalogue(Purpose::List(asker), roots);
            return;
        };
        if let Some(catalogue) = self.shared_skill_catalogue(chat) {
            self.load_skill(asker, catalogue, name, arguments);
            return;
        }
        let purpose = Purpose::Load {
            asker,
            name,
            arguments,
        };
        self.read_catalogue(purpose, roots);
    }

    /// Answers `asker`'s call with `result`, if the connection it came on
    /// is still the chat's, and the chat's tools are still on.
    fn answer_skill_tool(&mut self, asker: &Asker, result: Result<String, String>) {
        let Asker {
            token,
            id,
            chat,
            secret,
        } = asker;
        // A token can be reused after disconnect; never answer a different session.
        let same = matches!(
            self.sources.get(token.0),
            Some(Source::Tools(connection))
                if connection.chat == *chat && connection.token == *secret
        );
        if !same {
            return;
        }
        let off = (self.chat_ref(*chat))
            .is_none_or(|chat| chat.tools() == clackatoo_protocol::AgentTools::Off);
        let result = if off {
            Err("The chat's tools have been turned off.".into())
        } else {
            result
        };
        self.answer_tool(*token, id, result);
    }

    /// The catalogue the chat `chat` gets of `catalogue`, read for its
    /// roots: all of it, or what its specialist chooses.
    fn scoped_for(
        &self,
        chat: TerminalId,
        catalogue: &Arc<Catalogue>,
    ) -> Result<Arc<Catalogue>, crate::skills::Missing> {
        match self
            .chat_ref(chat)
            .and_then(|own| own.specialist_definition())
        {
            Some(specialist) => catalogue.scoped(specialist).map(Arc::new),
            None => Ok(Arc::clone(catalogue)),
        }
    }

    /// Gives `catalogue`, read for `roots`, to the chat `only`, or with
    /// none, to every chat with those roots, and returns the chats that
    /// can't have it, as their specialist needs what it doesn't hold.
    /// Each is told, and one that's connecting, as `purpose` says, fails
    /// to. Once for a read, however many wait on it: each chat given a
    /// catalogue has its commands and specialists worked out again.
    fn give_catalogue(
        &mut self,
        roots: &Roots,
        catalogue: &Arc<Catalogue>,
        only: Option<TerminalId>,
        purpose: &Purpose,
    ) -> BTreeSet<TerminalId> {
        let opening = |id: TerminalId| matches!(purpose, Purpose::Connect(chat) if *chat == id);
        self.give_catalogue_to(roots, catalogue, only, &opening)
    }

    /// As [`give_catalogue`](Self::give_catalogue), with `opening` saying
    /// which chats are connecting.
    fn give_catalogue_to(
        &mut self,
        roots: &Roots,
        catalogue: &Arc<Catalogue>,
        only: Option<TerminalId>,
        opening: &dyn Fn(TerminalId) -> bool,
    ) -> BTreeSet<TerminalId> {
        let mut blocked = BTreeSet::new();
        let chats: Vec<_> = match only {
            Some(chat) => vec![chat],
            None => self.terminals.keys().copied().collect(),
        };
        (self.skills.retries).retain(|retry| retry.roots != *roots);
        for id in chats {
            if self.chat_roots(id).as_ref() != Some(roots) {
                continue;
            }
            #[cfg(test)]
            {
                self.skills.given += 1;
            }
            match self.scoped_for(id, catalogue) {
                Ok(scoped) => {
                    let commands = scoped.commands();
                    let held = Held {
                        roots: roots.clone(),
                        catalogue: scoped,
                    };
                    self.skills.catalogues.insert(id, held);
                    let specialists = self.specialists_for(id);
                    if let Some(chat) = self.chat(id) {
                        chat.offer_skills(commands);
                        chat.offer_specialists(&specialists);
                    }
                }
                Err(missing) => {
                    blocked.insert(id);
                    self.forget_skill_catalogue(id);
                    if let Some(chat) = self.chat(id) {
                        if opening(id) {
                            chat.failed_to_connect(&missing.to_string());
                        } else {
                            chat.notify(missing.to_string());
                        }
                        chat.offer_skills(Vec::new());
                    }
                }
            }
            self.collect_damage(id);
        }
        blocked
    }

    /// A catalogue was read for `roots`: it's kept, and if what the
    /// user's own folders hold isn't what the last read for these roots
    /// found there, the catalogues kept for other roots are out of date
    /// too, and those of the chats that have them are read again. Those
    /// are then first reads, so they set off nothing more.
    fn catalogue_read(&mut self, roots: &Roots, catalogue: &Arc<Catalogue>) {
        let before = (self.skills.kept(roots)).map(|kept| kept.catalogue.personal());
        self.skills.keep(roots, catalogue);
        if before.is_some_and(|before| before != catalogue.personal()) {
            self.skills.kept.retain(|kept| kept.roots == *roots);
            self.refresh_skills_but(Some(roots));
        }
    }

    /// What was to be read for `purpose`, with `roots`, couldn't be, as
    /// `failed` says.
    fn skills_failed(&mut self, purpose: Purpose, roots: &Roots, failed: Failed) {
        self.skills_finished(purpose, roots, Err(failed), &BTreeSet::new());
    }

    /// What was read for `purpose`, with `roots`, is there, and has been
    /// given to the chats, but for those `blocked`; or it couldn't be
    /// read.
    fn skills_finished(
        &mut self,
        purpose: Purpose,
        roots: &Roots,
        result: Result<Arc<Catalogue>, Failed>,
        blocked: &BTreeSet<TerminalId>,
    ) {
        match purpose {
            Purpose::Refresh(_) => {
                if let Err(failed) = result {
                    self.refresh_failed(roots, &failed);
                }
            }
            Purpose::Connect(chat) => {
                if blocked.contains(&chat) {
                    return;
                }
                match result {
                    Ok(catalogue) => self.connect_with_skills(chat, &catalogue),
                    Err(failed) => self.connect_without_skills(chat, &failed),
                }
            }
            Purpose::Browse { token, ticket } => {
                let response = match result {
                    Ok(catalogue) => Response::Skills(catalogue.infos()),
                    Err(failed) => Response::Error(Error::Other(failed.to_string())),
                };
                self.hand_answer_with(token, ticket, response);
            }
            Purpose::BrowseSpecialists {
                token,
                ticket,
                files,
            } => {
                let response = match result {
                    Err(failed) => Response::Error(Error::Other(failed.to_string())),
                    Ok(catalogue) => {
                        let entries = Self::specialist_file_definitions(roots, &catalogue);
                        if files {
                            Response::SpecialistFiles(entries)
                        } else {
                            let definitions = entries.into_iter().map(|file| file.definition);
                            Response::Specialists(definitions.collect())
                        }
                    }
                };
                self.hand_answer_with(token, ticket, response);
            }
            // Never from roots the chat no longer has: what a folder no
            // longer trusted holds, or a skill switched off.
            Purpose::List(asker) | Purpose::Load { asker, .. }
                if result.is_ok() && self.chat_roots(asker.chat).as_ref() != Some(roots) =>
            {
                self.answer_skill_tool(&asker, Err(CHANGED.to_owned()));
            }
            Purpose::List(asker) => {
                let listed = result
                    .map_err(|failed| failed.to_string())
                    .and_then(|catalogue| {
                        let scoped = self.scoped_for(asker.chat, &catalogue);
                        scoped.map_err(|missing| missing.to_string())
                    })
                    .map(|catalogue| catalogue.listing());
                self.answer_skill_tool(&asker, listed);
            }
            Purpose::Load {
                asker,
                name,
                arguments,
            } => {
                let scoped = result
                    .map_err(|failed| failed.to_string())
                    .and_then(|catalogue| {
                        let scoped = self.scoped_for(asker.chat, &catalogue);
                        scoped.map_err(|missing| missing.to_string())
                    });
                match scoped {
                    Ok(catalogue) => self.load_skill(asker, catalogue, name, arguments),
                    Err(why) => self.answer_skill_tool(&asker, Err(why)),
                }
            }
            // Its skill comes from `load_skill`'s own job, to
            // `take_skills`: only that job failing ends here.
            Purpose::Loading(asker) => {
                if let Err(failed) = result {
                    self.answer_skill_tool(&asker, Err(failed.to_string()));
                }
            }
        }
    }

    /// Connects the chat `chat`, which has its catalogue. One just
    /// started in a project that isn't trusted is told its skills and
    /// specialists aren't loaded, as nothing else says so there.
    fn connect_with_skills(&mut self, chat: TerminalId, catalogue: &Catalogue) {
        if self.chat_ref(chat).is_none() {
            return;
        }
        self.connect(chat);
        let Some(own) = self.chat_ref(chat) else {
            return;
        };
        if own.node().turns.is_empty() && catalogue.holds_untrusted() {
            let project = super::tools::homely(own.project(), &self.config.home);
            let project = super::tools::visible(&project);
            let said = format!(
                "This project's skills and specialists aren't loaded, as its folder isn't \
                 trusted. :trust {project} trusts it."
            );
            if let Some(own) = self.chat(chat) {
                own.notify(said);
            }
            self.collect_damage(chat);
        }
    }

    /// The chat `chat`'s catalogue couldn't be read as it connects, as
    /// `failed` says: it connects without one, and is told. Nothing is
    /// kept for it, so it's read again the next time the chat connects:
    /// an empty catalogue kept would be taken for its skills having gone.
    /// One started as a specialist doesn't connect, as what its
    /// specialist needs can't be checked.
    fn connect_without_skills(&mut self, chat: TerminalId, failed: &Failed) {
        let specialist =
            (self.chat_ref(chat)).is_some_and(|own| own.specialist_definition().is_some());
        self.forget_skill_catalogue(chat);
        if specialist {
            if let Some(own) = self.chat(chat) {
                own.failed_to_connect(&failed.to_string());
            }
        } else if self.chat_ref(chat).is_some() {
            self.skills.skipped.insert(chat);
            self.connect(chat);
            self.skills.skipped.remove(&chat);
            if let Some(own) = self.chat(chat) {
                own.notify(format!(
                    "{failed} This chat goes on without shared skills until it connects again."
                ));
            }
        }
        self.collect_damage(chat);
    }

    /// When the first read under way will have taken too long.
    pub(super) fn next_skills_due(&self) -> Option<Instant> {
        self.skills
            .pending
            .values()
            .map(|pending| pending.due)
            .min()
    }

    /// Workers don't survive exec. Finish their RPCs before tool sockets
    /// and ordered client replies are handed to the next build. A chat
    /// waiting to connect is handed over as one that hasn't yet, and
    /// connects, reading its catalogue again, as any chat does after an
    /// upgrade: when it's shown or written to, or at once if it has a
    /// message waiting.
    pub(super) fn finish_skill_reads_for_upgrade(&mut self) {
        let pending = std::mem::take(&mut self.skills.pending);
        for (_, pending) in pending {
            let _ = self.work.scheduler.cancel(pending.work);
            if !matches!(&pending.purpose, Purpose::Connect(_) | Purpose::Refresh(_)) {
                self.skills_failed(pending.purpose, &pending.roots, Failed::Upgrading);
            }
        }
    }

    /// Whether the user cancelled `pending`'s read, in the list of
    /// background work.
    fn read_cancelled(&self, pending: &Pending) -> bool {
        matches!(
            self.work.scheduler.state(pending.work),
            Some(WorkState::Cancelling | WorkState::Cancelled)
        )
    }

    /// A skill was read for `waiting`, an agent's `load_skill`.
    fn skill_loaded(&mut self, waiting: Vec<Pending>, loaded: &Result<String, Unloadable>) {
        for pending in waiting {
            if self.read_cancelled(&pending) {
                self.skills_failed(pending.purpose, &pending.roots, Failed::Cancelled);
            } else if let Purpose::Loading(asker) = &pending.purpose {
                // Read from the catalogue the chat had when it asked: if
                // that's no longer the chat's, neither is the skill.
                let current = pending.epoch == self.skills.epoch
                    && self.chat_roots(asker.chat).as_ref() == Some(&pending.roots);
                let loaded = match loaded {
                    Ok(text) if current => Ok(text.clone()),
                    Ok(_) => Err(CHANGED.to_owned()),
                    Err(refused) => Err(refused.to_string()),
                };
                self.answer_skill_tool(asker, loaded);
            }
        }
    }

    /// `catalogue` was read, for everything in `waiting`, which all asked
    /// for the same roots. It's kept and given to the chats there once,
    /// however many waited, and then each thing waiting is answered: a
    /// daemon that starts with a hundred chats in one folder gives each
    /// its catalogue once, not once for each of the hundred.
    fn catalogue_arrived(&mut self, waiting: Vec<Pending>, catalogue: &Arc<Catalogue>) {
        let mut current = Vec::new();
        for pending in waiting {
            if self.read_cancelled(&pending) {
                self.skills_failed(pending.purpose, &pending.roots, Failed::Cancelled);
            } else if pending.epoch != self.skills.epoch {
                // Read before something was saved from Settings, or what
                // the user trusts or has switched off changed: again, and
                // for a chat, with its roots as they are now, not those
                // it asked with, which may say a folder is still trusted.
                let roots = (pending.purpose.chat())
                    .and_then(|chat| self.chat_roots(chat))
                    .unwrap_or(pending.roots);
                self.read_catalogue(pending.purpose, roots);
            } else {
                current.push(pending);
            }
        }
        let Some(roots) = current.first().map(|pending| pending.roots.clone()) else {
            return;
        };
        self.catalogue_read(&roots, catalogue);
        let connecting: BTreeSet<TerminalId> = (current.iter())
            .filter_map(|pending| match pending.purpose {
                Purpose::Connect(chat) => Some(chat),
                _ => None,
            })
            .collect();
        let opening = |chat: TerminalId| connecting.contains(&chat);
        let blocked = self.give_catalogue_to(&roots, catalogue, None, &opening);
        for pending in current {
            let read = Ok(Arc::clone(catalogue));
            self.skills_finished(pending.purpose, &roots, read, &blocked);
        }
    }

    /// Takes what the workers have read, and ends the reads that were
    /// cancelled, whose worker stopped, or that took too long.
    pub(super) fn take_skills(&mut self) {
        let done = std::mem::take(
            &mut *self
                .skills
                .done
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (reading, done) in done {
            let jobs: Vec<_> = (self.skills.pending.iter())
                .filter_map(|(&job, pending)| (pending.reading == reading).then_some(job))
                .collect();
            let waiting: Vec<Pending> = (jobs.into_iter())
                .filter_map(|job| self.skills.pending.remove(&job))
                .collect();
            match done {
                Done::Skill(loaded) => self.skill_loaded(waiting, &loaded),
                Done::Catalogue(catalogue) => self.catalogue_arrived(waiting, &catalogue),
            }
        }
        let now = Instant::now();
        let ended: Vec<_> = (self.skills.pending.iter())
            .filter_map(|(&job, pending)| {
                let failed = match self.work.scheduler.state(pending.work) {
                    Some(WorkState::Cancelled | WorkState::Cancelling) => Failed::Cancelled,
                    Some(WorkState::Failed) => Failed::Stopped,
                    _ if pending.due <= now => Failed::TimedOut,
                    _ => return None,
                };
                Some((job, failed))
            })
            .collect();
        for (job, failed) in ended {
            let pending = self.skills.pending.remove(&job).expect("listed above");
            let shared = (self.skills.pending.values()).any(|other| other.work == pending.work);
            if failed == Failed::TimedOut && !shared {
                let _ = self.work.scheduler.cancel(pending.work);
            }
            self.skills_failed(pending.purpose, &pending.roots, failed);
        }
        self.retry_refreshes();
        let Skills {
            catalogues,
            announced,
            skipped,
            ..
        } = &mut self.skills;
        catalogues.retain(|id, _| self.terminals.contains_key(id));
        announced.retain(|id, _| self.terminals.contains_key(id));
        skipped.retain(|id| self.terminals.contains_key(id));
    }

    /// What the chat `chat`'s agent is told of its skills, if anything.
    pub(super) fn skills_told(&self, chat: TerminalId) -> String {
        self.skills
            .catalogues
            .get(&chat)
            .map(|held| held.catalogue.instructions())
            .unwrap_or_default()
    }

    /// What goes to the chat `chat`'s agent with `action`, if it's a
    /// message that isn't a command of the agent's: its catalogue, and
    /// its role, the first time in the session `session`.
    pub(super) fn skill_brief_for(
        &mut self,
        chat: TerminalId,
        action: &crate::chat::Action,
        session: Option<&str>,
    ) -> Option<String> {
        use crate::chat::Action;
        let text = match action {
            Action::Prompt(prompt)
            | Action::Briefed { text: prompt, .. }
            | Action::Carry { text: prompt, .. } => &prompt.text,
            _ => return None,
        };
        if clackatoo_protocol::agent_command(text, &self.chat_ref(chat)?.node().commands) {
            return None;
        }
        let brief = self.skill_brief(chat, session?);
        if matches!(action, Action::Briefed { .. }) {
            let skills = if self.chat_ref(chat)?.tools() == clackatoo_protocol::AgentTools::Off {
                String::new()
            } else {
                self.skills_told(chat)
            };
            (!skills.is_empty()).then_some(skills)
        } else {
            brief
        }
    }

    /// Whether `text`, a message to the chat `chat`, invokes a shared
    /// skill: it names one, and not a command the chat's agent offers
    /// itself, which stays the agent's.
    pub(super) fn is_shared_skill(&self, chat: TerminalId, text: &str) -> bool {
        let named = (self.skills.catalogues.get(&chat))
            .is_some_and(|held| held.catalogue.has_command(text));
        named && !(self.chat_ref(chat)).is_some_and(|own| own.agent_offers(text))
    }

    /// A session's first ordinary prompt carries the catalogue too, for
    /// runtimes that don't put MCP server instructions into model context.
    /// Once for a session, and again once its context has been compacted,
    /// which loses it: never because the catalogue changed, as that would
    /// pay for it again in every chat, and the agent has `list_skills`.
    pub(super) fn skill_brief(&mut self, chat: TerminalId, session: &str) -> Option<String> {
        let own = self.chat_ref(chat)?;
        let tools = own.tools() != clackatoo_protocol::AgentTools::Off;
        let epoch = own.context_epoch;
        if self
            .skills
            .announced
            .get(&chat)
            .is_some_and(|(seen, before)| seen == session && *before == epoch)
        {
            return None;
        }
        self.skills.announced.insert(chat, (session.into(), epoch));
        let mut told = if tools {
            self.skills_told(chat)
        } else {
            String::new()
        };
        if let Some(specialist) = self
            .chat_ref(chat)
            .and_then(|chat| chat.specialist_definition())
        {
            let limits = self.chat_ref(chat).map_or(
                crate::specialists::Limits::None,
                crate::chat::HostedChat::limits,
            );
            if let Some(role) =
                crate::chat::role_for(&specialist.name, &specialist.instruction_text(), limits)
            {
                told.insert_str(0, &format!("{role}\n"));
            }
        }
        (!told.is_empty()).then_some(told)
    }
}

#[cfg(test)]
impl Daemon {
    /// Reads the chat `chat`'s catalogue here and now, and takes it as
    /// [`take_skills`](Self::take_skills) takes one a worker read.
    pub(super) fn read_skills_now(&mut self, chat: TerminalId) {
        let roots = self.chat_roots(chat).expect("a chat");
        let catalogue = Arc::new(Catalogue::discover(&roots));
        self.catalogue_read(&roots, &catalogue);
        self.give_catalogue(&roots, &catalogue, None, &Purpose::Refresh(None));
    }

    /// Lets go of the chat `chat`, as closing it does.
    pub(super) fn close_chat_for_test(&mut self, chat: TerminalId) {
        self.terminals.remove(&chat);
        self.forget_skill_catalogue(chat);
    }

    /// Makes every catalogue kept old enough to be read again.
    pub(super) fn age_skills(&mut self) {
        for kept in &mut self.skills.kept {
            kept.read = kept
                .read
                .checked_sub(FRESH)
                .expect("a clock past its start");
        }
    }
}

/// The words a message goes to the agent as: `before`, such as a role,
/// then `brief`, the catalogue, if it goes with this one, then `text`.
pub(super) fn words<'a>(before: &[&'a str], brief: Option<&'a str>, text: &'a str) -> Vec<&'a str> {
    before
        .iter()
        .copied()
        .chain(brief)
        .chain(std::iter::once(text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// Writes a skill called `name`, saying `body`, in the folder `skills`.
    fn skill(skills: &std::path::Path, name: &str, body: &str) {
        let folder = skills.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let text = format!("---\nname: {name}\ndescription: A procedure.\n---\n{body}\n");
        std::fs::write(folder.join("SKILL.md"), text).unwrap();
    }

    /// Adds a chat in `cwd` to `daemon`, with no agent or session yet.
    fn add_chat(daemon: &mut Daemon, cwd: &std::path::Path) -> TerminalId {
        // The chat the daemon was made with took its number without asking.
        let mut id = daemon.next_terminal();
        while daemon.terminals.contains_key(&id) {
            id = daemon.next_terminal();
        }
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let chat = crate::chat::HostedChat::new(
            id,
            clackatoo_protocol::Size::new(40, 10).unwrap(),
            cwd.display().to_string(),
            &agent,
            clackatoo_protocol::AgentTools::Ask,
            true,
            false,
        );
        daemon.add_chat(chat);
        id
    }

    /// Takes what the workers read until `done`, for five seconds at most.
    fn settle(daemon: &mut Daemon, done: impl Fn(&Daemon) -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !done(daemon) {
            daemon.take_skills();
            assert!(Instant::now() < until, "the catalogue was never read");
            std::thread::yield_now();
        }
    }

    #[test]
    fn shared_reads_complete_all_consumers_and_a_consumed_read_is_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let roots = daemon.skill_roots(dir.path().to_owned());
        // Simulate the final slice of a worker whose result was already
        // consumed. Its old key must not absorb a new request's closure.
        let (release, wait) = std::sync::mpsc::channel();
        let (began, started) = std::sync::mpsc::channel();
        let previous = daemon
            .work
            .scheduler
            .submit(
                "skills:0:1".into(),
                "Reading skills",
                Some(roots.cwd.to_string_lossy().into_owned()),
                Priority::Interactive,
                move |_| {
                    began.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(Step::Done)
                },
            )
            .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        daemon.skills.next = 1;
        daemon.read_catalogue(Purpose::Refresh(None), roots.clone());
        daemon.read_catalogue(Purpose::Refresh(None), roots);
        let work: Vec<_> = (daemon.skills.pending.values())
            .map(|pending| pending.work)
            .collect();
        assert_eq!(work.len(), 2);
        assert_eq!(work[0], work[1], "consumers share one catalogue scan");
        assert_ne!(work[0], previous, "the consumed result cannot be reused");
        release.send(()).unwrap();
        settle(&mut daemon, |daemon| daemon.skills.pending.is_empty());
    }

    #[test]
    fn a_kept_session_refreshes_its_skillsets_without_reopening_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let bundle = dir.path().join(".agents/skillsets/release");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("skillset.toml"),
            "format = 1\nname = \"release\"\ndescription = \"Release checks\"\n",
        )
        .unwrap();
        skill(&bundle.join("skills"), "check", "Check it.");
        daemon.refresh_chat_skills(chat);
        settle(&mut daemon, |daemon| daemon.is_shared_skill(chat, "/check"));
        let own = daemon.chat(chat).unwrap();
        assert!(own.is_connected());
        assert_eq!(own.session(), Some("s1"));
        let commands = &own.node().commands;
        assert!(commands.iter().any(|command| command.name == "check"));
    }

    /// As many chats as a daemon restarts with, all in one folder, wait on
    /// one read: the limit is on reads, not on what waits for them.
    #[test]
    fn any_number_of_chats_in_one_folder_share_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, first) = daemon_with_chat(dir.path());
        skill(&dir.path().join(".agents/skills"), "check", "Check it.");
        let mut chats = vec![first];
        for _ in 0..MOST_READING + 6 {
            chats.push(add_chat(&mut daemon, dir.path()));
        }
        for &chat in &chats {
            daemon.refresh_chat_skills(chat);
        }
        let reads: BTreeSet<u64> = (daemon.skills.pending.values())
            .map(|pending| pending.reading)
            .collect();
        assert_eq!(daemon.skills.pending.len(), chats.len());
        assert_eq!(reads.len(), 1);
        settle(&mut daemon, |daemon| daemon.skills.pending.is_empty());
        for chat in chats {
            assert!(daemon.is_shared_skill(chat, "/check"), "chat {chat}");
        }
    }

    /// Past the limit on reads, one for another folder is refused, and
    /// one for a folder being read still shares that read.
    #[test]
    fn reads_of_different_folders_are_limited() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = Arc::new(Mutex::new(wait));
        // Both workers held, so nothing queued is read meanwhile.
        for at in 0..2 {
            let wait = Arc::clone(&wait);
            let (began, started) = std::sync::mpsc::channel();
            let held = move |_: &crate::work::Context| {
                began.send(()).unwrap();
                let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(5));
                Ok(Step::Done)
            };
            // Each with a scope of its own, as one scope's work isn't done
            // by two workers at once.
            let key = format!("held:{at}");
            (daemon.work.scheduler)
                .submit(key.clone(), "Held", Some(key), Priority::Interactive, held)
                .unwrap();
            started.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let roots = |at: usize| daemon.skill_roots(dir.path().join(format!("folder-{at}")));
        let all: Vec<_> = (0..=MOST_READING).map(roots).collect();
        for roots in &all[..MOST_READING] {
            daemon.read_catalogue(Purpose::Refresh(None), roots.clone());
        }
        assert_eq!(daemon.skills.pending.len(), MOST_READING);
        daemon.read_catalogue(Purpose::Refresh(None), all[MOST_READING].clone());
        assert_eq!(daemon.skills.pending.len(), MOST_READING, "refused");
        daemon.read_catalogue(Purpose::Refresh(None), all[0].clone());
        assert_eq!(daemon.skills.pending.len(), MOST_READING + 1, "shared");
        drop(release);
        settle(&mut daemon, |daemon| daemon.skills.pending.is_empty());
    }

    #[test]
    fn missing_dependencies_cannot_be_bypassed_by_retrying_or_losing_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        (daemon.chat(chat).unwrap()).remember_specialist(crate::specialists::Specialist {
            name: "reviewer".into(),
            extra: crate::specialists::Extras {
                skills: Some(vec!["required-check".into()]),
                ..crate::specialists::Extras::default()
            },
            ..crate::specialists::Specialist::default()
        });
        let roots = daemon.chat_roots(chat).unwrap();
        let missing = Arc::new(Catalogue::discover(&roots));
        for _ in 0..2 {
            let blocked = daemon.give_catalogue(&roots, &missing, None, &Purpose::Connect(chat));
            assert!(blocked.contains(&chat));
            assert!(daemon.skill_catalogue(chat).is_none());
        }
        daemon.skills_failed(Purpose::Connect(chat), &roots, Failed::TimedOut);
        assert!(daemon.skill_catalogue(chat).is_none());
        skill(
            &dir.path().join(".agents/skills"),
            "required-check",
            "Check",
        );
        let fixed = Arc::new(Catalogue::discover(&roots));
        let blocked = daemon.give_catalogue(&roots, &fixed, None, &Purpose::Connect(chat));
        assert!(blocked.is_empty());
        assert!(daemon.is_shared_skill(chat, "/required-check"));
    }

    /// A read that took too long leaves the chat with no catalogue,
    /// rather than an empty one that would stand for its skills having
    /// gone: the chat connects without, and its next connection reads
    /// again.
    #[test]
    fn a_read_that_fails_keeps_nothing_and_is_made_again_at_the_next_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        skill(&dir.path().join(".agents/skills"), "check", "Check it.");
        let roots = daemon.chat_roots(chat).unwrap();
        daemon.skills_failed(Purpose::Connect(chat), &roots, Failed::TimedOut);
        assert!(daemon.skill_catalogue(chat).is_none());
        assert!(daemon.skills.skipped.is_empty());
        let notice = daemon.chat(chat).unwrap().node().notice.clone().unwrap();
        assert!(notice.contains("took too long"), "{notice}");
        assert!(notice.contains("without shared skills"), "{notice}");
        // The next time it connects, it waits for a read again.
        assert!(daemon.reading_skills(chat));
        settle(&mut daemon, |daemon| daemon.skill_catalogue(chat).is_some());
        assert!(daemon.is_shared_skill(chat, "/check"));
        assert!(!daemon.reading_skills(chat));
    }

    /// What's in the user's own folders is every chat's: a read for one
    /// folder that finds them changed has the other folders' chats read
    /// theirs again.
    #[test]
    fn a_change_in_the_users_own_folders_reaches_chats_in_every_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, here) = daemon_with_chat(dir.path());
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let there = add_chat(&mut daemon, &repo);
        daemon.read_skills_now(here);
        daemon.read_skills_now(there);
        assert_ne!(daemon.chat_roots(here), daemon.chat_roots(there));
        assert!(!daemon.is_shared_skill(here, "/added"));

        // A project's skill is only that project's chats'.
        skill(&repo.join(".agents/skills"), "local", "Local.");
        daemon.age_skills();
        daemon.refresh_chat_skills(there);
        settle(&mut daemon, |daemon| {
            daemon.is_shared_skill(there, "/local")
        });
        assert!(daemon.skills.pending.is_empty());
        assert!(!daemon.is_shared_skill(here, "/local"));

        skill(&dir.path().join(".agents/skills"), "added", "Added.");
        daemon.age_skills();
        daemon.refresh_chat_skills(there);
        settle(&mut daemon, |daemon| daemon.is_shared_skill(here, "/added"));
        assert!(daemon.is_shared_skill(there, "/added"));
        settle(&mut daemon, |daemon| daemon.skills.pending.is_empty());
    }

    /// A catalogue read a moment ago isn't read again, whatever asks.
    #[test]
    fn a_catalogue_just_read_is_used_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        daemon.read_skills_now(chat);
        skill(&dir.path().join(".agents/skills"), "late", "Late.");
        daemon.refresh_chat_skills(chat);
        assert!(daemon.skills.pending.is_empty(), "no read was started");
        assert!(!daemon.is_shared_skill(chat, "/late"));
        daemon.age_skills();
        daemon.refresh_chat_skills(chat);
        assert_eq!(daemon.skills.pending.len(), 1);
        settle(&mut daemon, |daemon| daemon.is_shared_skill(chat, "/late"));
    }

    /// Settings asks for the MCP servers every few seconds while it's
    /// open. A catalogue that offers what the last did isn't given to the
    /// chats again; one that offers something new is.
    #[test]
    fn a_catalogue_that_changes_nothing_isnt_given_to_chats_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        daemon.read_skills_now(chat);
        let roots = daemon.chat_roots(chat).unwrap();
        let before = daemon.shared_skill_catalogue(chat).unwrap();
        let epoch = daemon.skills.epoch;
        daemon.refresh_mcp_catalogue(&roots, Catalogue::discover(&roots));
        let after = daemon.shared_skill_catalogue(chat).unwrap();
        assert!(Arc::ptr_eq(&before, &after));
        assert_eq!(daemon.skills.epoch, epoch);
        skill(&dir.path().join(".agents/skills"), "new", "New.");
        daemon.refresh_mcp_catalogue(&roots, Catalogue::discover(&roots));
        assert!(daemon.is_shared_skill(chat, "/new"));
    }

    /// A command the agent offers itself stays the agent's: a shared
    /// skill of the same name isn't offered as a command, nor read when
    /// the command is typed.
    #[test]
    fn a_shared_skill_doesnt_take_the_place_of_the_agents_own_command() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let skills = dir.path().join(".agents/skills");
        skill(&skills, "check", "Shared.");
        skill(&skills, "other", "Shared.");
        daemon.read_skills_now(chat);
        assert!(daemon.is_shared_skill(chat, "/check now"));
        let update = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": [{ "name": "check", "description": "The agent's own" }],
        }))
        .unwrap();
        daemon.chat(chat).unwrap().update(update);
        assert!(!daemon.is_shared_skill(chat, "/check now"));
        assert!(!daemon.is_shared_skill(chat, "/CHECK"));
        assert!(daemon.is_shared_skill(chat, "/other"));
        let commands = &daemon.chat(chat).unwrap().node().commands;
        let offered: Vec<_> = (commands.iter())
            .map(|command| (command.name.as_str(), command.description.as_str()))
            .collect();
        assert_eq!(
            offered,
            [("other", "A procedure."), ("check", "The agent's own")]
        );
    }

    /// A chat in a project that isn't trusted is offered nothing of the
    /// project's, until the folder is trusted, which reads its chats'
    /// catalogues again.
    #[test]
    fn a_projects_skills_follow_the_folders_trust() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        daemon.trust.folders = crate::trust::Trust::load(&crate::Trusting::ThisRun);
        let repo = dir.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        skill(&repo.join(".agents/skills"), "local", "Local.");
        let chat = add_chat(&mut daemon, &repo);
        assert!(!daemon.chat_roots(chat).unwrap().trusted);
        daemon.read_skills_now(chat);
        assert!(!daemon.is_shared_skill(chat, "/local"));
        assert!(daemon.skill_catalogue(chat).unwrap().holds_untrusted());

        daemon.trust.folders.mark(repo.clone(), true);
        daemon.skills_changed();
        assert!(daemon.chat_roots(chat).unwrap().trusted);
        settle(&mut daemon, |daemon| daemon.is_shared_skill(chat, "/local"));
        // A folder in it marked not trusted is still not read.
        let inner = repo.join("vendor");
        std::fs::create_dir_all(&inner).unwrap();
        daemon.trust.folders.mark(inner.clone(), false);
        let vendored = add_chat(&mut daemon, &inner);
        assert!(!daemon.chat_roots(vendored).unwrap().trusted);
    }

    /// The agent is told its catalogue once in a session, and again after
    /// its context is compacted, which loses it: not because the
    /// catalogue changed.
    #[test]
    fn the_catalogue_is_announced_once_and_again_only_after_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let skills = dir.path().join(".agents/skills");
        skill(&skills, "check", "Check it.");
        daemon.read_skills_now(chat);
        assert!(daemon.skill_brief(chat, "s1").unwrap().contains("- check:"));
        assert!(daemon.skill_brief(chat, "s1").is_none());
        skill(&skills, "added", "Added.");
        daemon.read_skills_now(chat);
        assert!(daemon.is_shared_skill(chat, "/added"));
        assert!(daemon.skill_brief(chat, "s1").is_none(), "not sent again");
        daemon.chat(chat).unwrap().context_epoch += 1;
        assert!(daemon.skill_brief(chat, "s1").unwrap().contains("- added:"));
        assert!(daemon.skill_brief(chat, "s2").is_some(), "a new session");
    }

    #[test]
    fn active_role_returns_after_compaction_even_with_tools_off() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let own = daemon.chat(chat).unwrap();
        own.remember_specialist(crate::specialists::Specialist {
            name: "reviewer".into(),
            instructions: "ACTIVE REVIEW ROLE".into(),
            ..crate::specialists::Specialist::default()
        });
        own.act(clackatoo_protocol::ChatAction::Set {
            setting: "clackatoo.tools".into(),
            value: "off".into(),
        });
        let told = daemon.skill_brief(chat, "s1").unwrap();
        assert!(told.contains("ACTIVE REVIEW ROLE"));
        assert!(daemon.skill_brief(chat, "s1").is_none());
        daemon.chat(chat).unwrap().context_epoch += 1;
        let told = daemon.skill_brief(chat, "s1").unwrap();
        assert!(told.contains("ACTIVE REVIEW ROLE"));
    }

    /// The read-only specialists clackatoo builds in choose no skills, so
    /// the chats agents start as them, often many at once, aren't each
    /// told the catalogue.
    #[test]
    fn the_built_in_read_only_specialists_are_told_no_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        skill(&dir.path().join(".agents/skills"), "check", "Check it.");
        for specialist in crate::specialists::built_in() {
            let name = specialist.name.clone();
            daemon.chat(chat).unwrap().remember_specialist(specialist);
            daemon.read_skills_now(chat);
            let told = daemon.skills_told(chat);
            if name == crate::specialists::DEFAULT {
                assert!(told.contains("- check:"), "{name}: {told}");
            } else {
                assert_eq!(told, "", "{name}");
            }
        }
    }

    /// A folder untrusted, or a skill switched off, stops being offered
    /// there and then, before anything is read again, and so whether or
    /// not that read succeeds. A read that fails is made again.
    #[test]
    fn untrusting_or_switching_off_takes_effect_before_the_read_that_follows() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        daemon.trust.folders = crate::trust::Trust::load(&crate::Trusting::ThisRun);
        let repo = dir.path().canonicalize().unwrap().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        skill(&repo.join(".agents/skills"), "local", "Local.");
        skill(&dir.path().join(".agents/skills"), "mine", "Mine.");
        daemon.trust.folders.mark(repo.clone(), true);
        let chat = add_chat(&mut daemon, &repo);
        daemon.read_skills_now(chat);
        let offers = |daemon: &mut Daemon, name: &str| {
            let commands = &daemon.chat(chat).unwrap().node().commands;
            commands.iter().any(|command| command.name == name)
        };
        assert!(daemon.is_shared_skill(chat, "/local") && offers(&mut daemon, "local"));

        daemon.trust.folders.mark(repo.clone(), false);
        daemon.skills_changed();
        assert!(daemon.skill_catalogue(chat).is_none(), "gone at once");
        assert!(!daemon.is_shared_skill(chat, "/local") && !offers(&mut daemon, "local"));
        // The read that follows fails, as one that takes too long does.
        let roots = daemon.chat_roots(chat).unwrap();
        let pending = std::mem::take(&mut daemon.skills.pending);
        assert!(!pending.is_empty());
        daemon.skills_failed(Purpose::Refresh(None), &roots, Failed::TimedOut);
        assert!(daemon.skill_catalogue(chat).is_none());
        // It's made again, and gives the chat what it may still have.
        daemon.take_skills();
        assert_eq!(daemon.skills.pending.len(), 1, "read again");
        settle(&mut daemon, |daemon| daemon.skill_catalogue(chat).is_some());
        assert!(daemon.is_shared_skill(chat, "/mine"));
        assert!(!daemon.is_shared_skill(chat, "/local"));
        assert!(daemon.skills.retries.is_empty());

        let off: Arc<[String]> = ["mine".to_owned()].into();
        daemon.config.agents.configured.disabled_skills = off;
        daemon.skills_changed();
        assert!(!daemon.is_shared_skill(chat, "/mine") && !offers(&mut daemon, "mine"));
        // Tried only so many times, when every read fails.
        for _ in 0..MOST_RETRIES {
            let roots = daemon.chat_roots(chat).unwrap();
            daemon.skills.pending.clear();
            daemon.skills_failed(Purpose::Refresh(None), &roots, Failed::TimedOut);
            daemon.take_skills();
        }
        assert!(daemon.skills.retries.is_empty());
        assert!(daemon.skill_catalogue(chat).is_none());
    }

    /// A read many chats wait on gives each chat its catalogue once: the
    /// work on the event loop grows with the chats, not with their square.
    #[test]
    fn a_read_many_chats_share_gives_each_its_catalogue_once() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, first) = daemon_with_chat(dir.path());
        skill(&dir.path().join(".agents/skills"), "check", "Check it.");
        let mut chats = vec![first];
        for _ in 1..64 {
            chats.push(add_chat(&mut daemon, dir.path()));
        }
        for &chat in &chats {
            daemon.refresh_chat_skills(chat);
        }
        settle(&mut daemon, |daemon| daemon.skills.pending.is_empty());
        assert_eq!(daemon.skills.given, 64);
        // One more that asks while it's fresh is given it alone.
        let late = add_chat(&mut daemon, dir.path());
        daemon.refresh_chat_skills(late);
        assert!(daemon.is_shared_skill(late, "/check"));
        assert_eq!(daemon.skills.given, 65);
    }

    /// Settings' catalogue for a folder is the one of a chat there, by
    /// whatever name the folder is reached, and the project found for a
    /// folder with no chat is kept a while rather than looked for on
    /// disk at each of Settings' requests.
    #[test]
    fn a_folders_roots_are_a_chats_there_and_its_project_is_kept_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let top = dir.path().canonicalize().unwrap();
        let repo = top.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let link = top.join("link");
        std::os::unix::fs::symlink(repo.join("sub"), &link).unwrap();
        let through = daemon.skill_roots(link.clone());
        assert_eq!(through.cwd, repo.join("sub"));
        assert_eq!(through.project, repo);
        let chat = add_chat(&mut daemon, &repo.join("sub"));
        assert_eq!(
            Some(daemon.skill_roots(link.clone())),
            daemon.chat_roots(chat)
        );
        daemon.close_chat_for_test(chat);
        // Not looked for again: the repository going isn't seen yet.
        std::fs::remove_dir(repo.join(".git")).unwrap();
        assert_eq!(daemon.skill_roots(link.clone()).project, repo);
        // A save looks afresh, as it writes there.
        assert_eq!(
            daemon.skill_roots_now(link.clone()).project,
            repo.join("sub")
        );
        std::fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(daemon.skill_roots(link.clone()).project, repo.join("sub"));
        for looked in daemon.skills.looked.lock().unwrap().iter_mut() {
            looked.at = looked.at.checked_sub(PROJECT_KNOWN_FOR).unwrap();
        }
        assert_eq!(daemon.skill_roots(link).project, repo);
        // Each of several folders is kept, not only the last.
        for name in ["one", "two"] {
            std::fs::create_dir_all(top.join(name).join(".git")).unwrap();
            assert_eq!(daemon.skill_roots(top.join(name)).project, top.join(name));
        }
        std::fs::remove_dir(top.join("one/.git")).unwrap();
        assert_eq!(daemon.skill_roots(top.join("one")).project, top.join("one"));
        assert_eq!(daemon.skills.looked.lock().unwrap().len(), 3);
    }

    /// Connects a tools server for the chat `chat`, as its agent's would
    /// be, and returns its token and the end its answers are read from.
    fn tools_of(daemon: &mut Daemon, chat: TerminalId) -> (Token, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        ours.set_nonblocking(true).unwrap();
        let stream = mio::net::UnixStream::from_std(ours);
        let info = (String::new(), false);
        let connection =
            crate::mcp::Connection::new(stream, chat, Vec::new(), info, &daemon.parser);
        let token = Token(daemon.sources.insert(Source::Tools(connection)));
        (token, theirs)
    }

    /// The next answer to a tool call on `theirs`: whether it's an error,
    /// and its text.
    fn answer(theirs: &std::os::unix::net::UnixStream) -> (bool, String) {
        use std::io::BufRead as _;
        let mut said = String::new();
        std::io::BufReader::new(theirs)
            .read_line(&mut said)
            .unwrap();
        let answer: Value = serde_json::from_str(&said).unwrap();
        let error = answer["result"]["isError"] == true;
        (error, answer["result"]["content"].to_string())
    }

    /// A trusted repository with a skill of its own, `local`, and a chat
    /// in it, with its tools connected.
    fn trusted_chat(
        dir: &std::path::Path,
    ) -> (
        Daemon,
        TerminalId,
        PathBuf,
        Token,
        std::os::unix::net::UnixStream,
    ) {
        let (mut daemon, _) = daemon_with_chat(dir);
        daemon.trust.folders = crate::trust::Trust::load(&crate::Trusting::ThisRun);
        let repo = dir.canonicalize().unwrap().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        skill(&repo.join(".agents/skills"), "local", "LOCAL PROCEDURE");
        skill(&dir.join(".agents/skills"), "mine", "MY PROCEDURE");
        daemon.trust.folders.mark(repo.clone(), true);
        let chat = add_chat(&mut daemon, &repo);
        let (token, theirs) = tools_of(&mut daemon, chat);
        (daemon, chat, repo, token, theirs)
    }

    /// A `list_skills` being read as its folder is untrusted is read
    /// again for the chat as it is now, and never answered with what the
    /// folder holds.
    #[test]
    fn a_list_in_flight_as_its_folder_is_untrusted_names_nothing_of_the_folders() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat, repo, token, theirs) = trusted_chat(dir.path());
        daemon.skill_tool(token, serde_json::json!(7), chat, None);
        let trusted: Vec<_> = (daemon.skills.pending.values())
            .map(|pending| pending.roots.trusted)
            .collect();
        assert_eq!(trusted, [true]);
        daemon.trust.folders.mark(repo, false);
        daemon.skills_changed();
        settle(&mut daemon, |daemon| {
            !(daemon.skills.pending.values())
                .any(|pending| matches!(pending.purpose, Purpose::List(_)))
        });
        let (error, listed) = answer(&theirs);
        assert!(!error, "{listed}");
        assert!(
            listed.contains("mine") && !listed.contains("local"),
            "{listed}"
        );
        // And were the old read answered after all, it says to ask again.
        let stale = Roots {
            trusted: true,
            ..daemon.chat_roots(chat).unwrap()
        };
        let catalogue = Arc::new(Catalogue::discover(&stale));
        let asker = Asker {
            token,
            id: serde_json::json!(8),
            chat,
            secret: None,
        };
        daemon.skills_finished(
            Purpose::List(asker),
            &stale,
            Ok(catalogue),
            &BTreeSet::new(),
        );
        let (error, said) = answer(&theirs);
        assert!(error && said.contains("Ask again"), "{said}");
        assert!(!said.contains("local"));
    }

    /// A `load_skill` being read as its folder is untrusted, or as the
    /// skill is switched off, isn't answered with the skill.
    #[test]
    fn a_load_in_flight_as_its_skill_is_taken_away_isnt_answered_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat, repo, token, theirs) = trusted_chat(dir.path());
        daemon.read_skills_now(chat);
        let load = |daemon: &mut Daemon, id: u32, name: &str| {
            let load = Some((name.to_owned(), String::new()));
            daemon.skill_tool(token, serde_json::json!(id), chat, load);
            assert_eq!(daemon.skills.pending.len(), 1);
        };
        let loaded = |daemon: &mut Daemon| {
            settle(daemon, |daemon| {
                !(daemon.skills.pending.values())
                    .any(|pending| matches!(pending.purpose, Purpose::Loading(_)))
            });
            answer(&theirs)
        };
        load(&mut daemon, 1, "local");
        let (error, said) = loaded(&mut daemon);
        assert!(!error && said.contains("LOCAL PROCEDURE"), "{said}");

        load(&mut daemon, 2, "local");
        daemon.trust.folders.mark(repo, false);
        daemon.skills_changed();
        let (error, said) = loaded(&mut daemon);
        assert!(error && said.contains("Ask again"), "{said}");
        assert!(!said.contains("LOCAL PROCEDURE"));

        settle(&mut daemon, |daemon| daemon.skill_catalogue(chat).is_some());
        load(&mut daemon, 3, "mine");
        let off: Arc<[String]> = ["mine".to_owned()].into();
        daemon.config.agents.configured.disabled_skills = off;
        daemon.skills_changed();
        let (error, said) = loaded(&mut daemon);
        assert!(error && said.contains("Ask again"), "{said}");
        assert!(!said.contains("MY PROCEDURE"));
    }

    /// While the home directory can't be read, it's the one in the
    /// environment, and looked for again: what's kept is only what was
    /// found.
    #[test]
    fn a_home_that_cant_be_read_yet_is_looked_for_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let top = dir.path().canonicalize().unwrap();
        let (real, link) = (top.join("mounted"), top.join("home"));
        std::os::unix::fs::symlink(&real, &link).unwrap();
        daemon.config.home.clone_from(&link);
        daemon.skills.home = std::sync::OnceLock::new();
        assert_eq!(daemon.real_home(), link, "not there yet");
        std::fs::create_dir(&real).unwrap();
        assert_eq!(daemon.real_home(), real);
        std::fs::remove_dir(&real).unwrap();
        assert_eq!(daemon.real_home(), real, "found once");
    }
}
