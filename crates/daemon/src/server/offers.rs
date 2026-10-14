//! Offering to install a buffer's language: its grammar, if no pack is for
//! its file but clackatoo knows one, and its language server, if that's
//! nowhere to be found but clackatoo knows where to get it.
//!
//! Each client is offered each language once, when it attaches to a
//! buffer of it, unless the answer was once no: then it's listed in the
//! declined file and never offered again. What to do with an offer is
//! the client's to decide, as its `languageinstall` option says.
//!
//! An install runs on a thread of its own, as downloading takes a while,
//! and wakes the event loop when it ends. Then every buffer shown is
//! parsed, and opened in its server, if it couldn't be before.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use clackatoo_protocol::{BufferId, BufferUpdate, Event, Request};
use clackatoo_syntax::servers::{find_program, known_server};
use mio::Token;

use super::Daemon;

/// The installs offered and under way.
#[derive(Debug)]
pub(super) struct Installs {
    /// The languages each client has been offered.
    offered: HashSet<(Token, String)>,
    /// The languages never to offer.
    declined: Declined,
    /// The languages being installed, and the clients to tell when they
    /// are.
    installing: HashMap<String, Vec<Token>>,
    /// Installs that have ended, put here by their threads.
    ended: Arc<Mutex<Vec<Ended>>>,
}

/// An install that's ended.
#[derive(Debug)]
struct Ended {
    language: String,
    /// The language server's program, if one was installed.
    server: Option<String>,
    /// What was installed, or why it couldn't be.
    outcome: Result<String, String>,
}

impl Installs {
    /// Installs with none under way, declining what the file at `declined`
    /// lists.
    pub(super) fn new(declined: Option<&Path>) -> Self {
        Self {
            offered: HashSet::new(),
            declined: Declined::new(declined),
            installing: HashMap::new(),
            ended: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Forgets the client at `token`, as it's gone: what it was offered,
    /// and that it's waiting to hear about installs.
    pub(super) fn forget(&mut self, token: Token) {
        self.offered.retain(|(offered, _)| *offered != token);
        for waiting in self.installing.values_mut() {
            waiting.retain(|&waiting| waiting != token);
        }
    }
}

/// The languages never to offer, as the declined file lists them, one a
/// line. The file is read again when it's changed, as when
/// `clackatoo language install` takes a language off it.
#[derive(Debug)]
struct Declined {
    path: Option<PathBuf>,
    languages: HashSet<String>,
    /// When the file was last changed, as of reading it.
    read: Option<SystemTime>,
}

impl Declined {
    fn new(path: Option<&Path>) -> Self {
        let mut declined = Self {
            path: path.map(Path::to_owned),
            languages: HashSet::new(),
            read: None,
        };
        declined.refresh();
        declined
    }

    fn refresh(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        let changed = fs::metadata(path).and_then(|meta| meta.modified()).ok();
        if changed.is_some() && changed == self.read {
            return;
        }
        self.read = changed;
        let text = fs::read_to_string(path).unwrap_or_default();
        self.languages = (text.lines().map(str::trim))
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
    }

    fn contains(&mut self, language: &str) -> bool {
        self.refresh();
        self.languages.contains(language)
    }

    /// Adds `language`, writing the file.
    fn add(&mut self, language: String) {
        self.refresh();
        if !self.languages.insert(language) {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };
        let mut languages: Vec<&String> = self.languages.iter().collect();
        languages.sort();
        let text: String = (languages.iter())
            .flat_map(|language| [language.as_str(), "\n"])
            .collect();
        if let Err(err) = fs::write(path, text) {
            log::warn!("{} couldn't be written: {err}", path.display());
        }
        self.read = fs::metadata(path).and_then(|meta| meta.modified()).ok();
    }
}

/// Held by each install while it runs, so two never write the same
/// server's directory at once, as installing JavaScript and TypeScript
/// together would: both have typescript-language-server.
static INSTALLING: Mutex<()> = Mutex::new(());

impl Daemon {
    /// Offers the client at `token` to install buffer `id`'s language, or
    /// its language server, if either is missing and can be installed, and
    /// it hasn't been offered or declined.
    pub(super) fn offer_install(&mut self, token: Token, id: BufferId) {
        let (Some(languages), Some(servers), Some(slot)) = (
            &mut self.languages,
            self.config.servers.as_deref(),
            self.buffers.get(&id),
        ) else {
            return;
        };
        let path = slot.buffer.path();
        let missing = |command: Option<&Vec<String>>| {
            let program = command?.first()?;
            let known = known_server(program)?;
            (find_program(program, servers).is_none() && known.installable())
                .then(|| program.clone())
        };
        let (language, grammar, server) = if let Some(pack) = languages.find(path) {
            let server = missing(pack.server.as_ref().map(|server| &server.command));
            (pack.name.clone(), false, server)
        } else if let Some(known) = clackatoo_syntax::known_for(path) {
            let usual = clackatoo_syntax::Server::of(known.name, None, None);
            (
                known.name.to_owned(),
                true,
                missing(usual.as_ref().map(|usual| &usual.command)),
            )
        } else {
            return;
        };
        let installs = &mut self.installs;
        if !grammar && server.is_none()
            || installs.declined.contains(&language)
            || installs.installing.contains_key(&language)
            || !installs.offered.insert((token, language.clone()))
        {
            return;
        }
        log::info!("buffer {id}: offering to install {language}");
        let offer = BufferUpdate::Offer {
            language,
            grammar,
            server,
        };
        self.tell(token, id, offer);
    }

    /// Acts on an install or a decline from the client at `token`.
    pub(super) fn respond_about_installs(&mut self, token: Token, request: Request) {
        match request {
            Request::Install {
                language,
                grammar,
                server,
            } => self.install(token, language, grammar, server),
            Request::Decline { language } => self.decline(language),
            _ => {}
        }
    }

    /// Installs `language`'s grammar if `grammar`, and the language server
    /// `server`, telling the client at `token` how it went when it's done.
    fn install(&mut self, token: Token, language: String, grammar: bool, server: Option<String>) {
        if let Some(waiting) = self.installs.installing.get_mut(&language) {
            // Already under way, for another window.
            waiting.push(token);
            return;
        }
        let (Some(languages), Some(servers)) =
            (self.config.languages.clone(), self.config.servers.clone())
        else {
            let outcome = Err("this daemon doesn't install languages".to_owned());
            self.send_event(token, Event::Installed { language, outcome });
            return;
        };
        log::info!("installing {language}");
        self.installs
            .installing
            .insert(language.clone(), vec![token]);
        let ended = Arc::clone(&self.installs.ended);
        let waker = Arc::clone(&self.waker);
        let language_again = language.clone();
        let spawned = std::thread::Builder::new()
            .name("install".into())
            .spawn(move || {
                let outcome = install(&language, grammar, server.as_deref(), &languages, &servers);
                let ended_install = Ended {
                    language,
                    server,
                    outcome,
                };
                ended
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(ended_install);
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for an install failed: {err}");
                }
            });
        if let Err(err) = spawned {
            log::error!("an install couldn't start: {err}");
            let tokens = (self.installs.installing.remove(&language_again)).unwrap_or_default();
            for token in tokens {
                let outcome = Err(format!("the install couldn't start: {err}"));
                let language = language_again.clone();
                self.send_event(token, Event::Installed { language, outcome });
            }
        }
    }

    /// Never offers `language` again, and lists it in the declined file.
    fn decline(&mut self, language: String) {
        self.installs.declined.add(language);
    }

    /// Tells the windows that asked how the installs that have ended went,
    /// and parses and opens in their servers the buffers shown that
    /// couldn't be before.
    pub(super) fn take_installs(&mut self) {
        if self.installs.installing.is_empty() {
            return;
        }
        let ended = std::mem::take(
            &mut *(self.installs.ended.lock()).unwrap_or_else(PoisonError::into_inner),
        );
        if ended.is_empty() {
            return;
        }
        for Ended {
            language,
            server,
            outcome,
        } in ended
        {
            log::info!("installing {language}: {outcome:?}");
            let tokens = self
                .installs
                .installing
                .remove(&language)
                .unwrap_or_default();
            if let Some(program) = server.filter(|_| outcome.is_ok()) {
                // Not found before, so not looked for again: now it's there.
                self.servers.forget_starts(&program);
            }
            for token in tokens {
                let outcome = outcome.clone();
                let event = Event::Installed {
                    language: language.clone(),
                    outcome,
                };
                self.send_event(token, event);
            }
        }
        let shown: Vec<BufferId> = (self.buffers.iter())
            .filter(|(_, slot)| !slot.viewers.is_empty())
            .map(|(&id, _)| id)
            .collect();
        for id in shown {
            // A pack installed may say other of the words of one parsed.
            if self.buffers.get(&id).is_some_and(|slot| !slot.parsed) {
                self.parse_buffer(id);
            } else {
                self.find_words(id);
            }
            self.open_in_server(id);
        }
    }
}

/// Installs `language`'s grammar, if `grammar`, in `languages`, and the
/// language server `server` in `servers`, saying what was installed, or
/// why it couldn't be.
fn install(
    language: &str,
    grammar: bool,
    server: Option<&str>,
    languages: &Path,
    servers: &Path,
) -> Result<String, String> {
    let _one_at_a_time = INSTALLING.lock().unwrap_or_else(PoisonError::into_inner);
    let mut installed = Vec::new();
    if grammar {
        let known = clackatoo_syntax::known(language)
            .ok_or_else(|| format!("clackatoo doesn't know where to get {language}"))?;
        clackatoo_syntax::install(known, languages)
            .map_err(|err| format!("{language}'s grammar couldn't be installed: {err}"))?;
        installed.push(format!("{language}'s grammar"));
    }
    if let Some(program) = server {
        if find_program(program, servers).is_some() {
            // Installed meanwhile, for another language.
            installed.push(format!("{program} (installed already)"));
            return Ok(installed.join(" and "));
        }
        let known = known_server(program)
            .ok_or_else(|| format!("clackatoo doesn't know where to get {program}"))?;
        let result = known.install(servers);
        result.map_err(|err| match installed.first() {
            Some(done) => format!("{done} is installed, but {program} couldn't be: {err}"),
            None => format!("{program} couldn't be installed: {err}"),
        })?;
        installed.push(program.to_owned());
    }
    Ok(installed.join(" and "))
}
