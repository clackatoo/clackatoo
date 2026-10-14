//! Language servers in the event loop: starting one for each project a
//! buffer is in, keeping the buffer open in it as it's edited, and sending
//! what it says is wrong to every client following the buffer.
//!
//! One server runs for each command and project root, so every buffer in
//! a project shares one, as do languages with the same server, such as
//! JavaScript and TypeScript. A server goes a while after its last buffer
//! closes, asked to shut down as LSP asks (see [`super::stopping`]), and
//! doesn't come through an upgrade: the next build starts it again when a
//! buffer of its is next attached to. The files it asks to hear of are
//! watched for it (see [`super::watched`]).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    Answer, BufferId, BufferUpdate, ClientMessage, CompletionItem, Edit, FileLocation, Query,
    ServerFeatures, TextChange,
};
use clackatoo_text::{Bias, Diagnostic, Diagnostics, Rope};
use mio::unix::SourceFd;
use mio::{Interest, Token};
use serde_json::{Value, json};

use super::buffers::BufferSlot;
use super::groups::End;
use super::rename::{Made, Purpose};
use super::stopping::{EXIT_WAIT, Stopping};
use super::watched::{Walking, Wanted};
use super::{Daemon, OUTPUT_BUDGET, Orphan, Source};
use crate::lsp::{self, Incoming, LanguageServer, Reported};
use crate::pty::Process;

/// Identifies a language server within one daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ServerId(u32);

/// Which of a language server's pipes a token is for.
#[derive(Clone, Copy, Debug)]
pub(super) enum Pipe {
    /// Its stdout, where its messages come.
    Output,
    /// Its stderr.
    Errors,
    /// Its stdin, watched for room to write.
    Input,
    /// Its pidfd.
    Exit,
}

/// How many times a server is started for one project, before it's left
/// stopped: once, and once again after it stops by itself.
const STARTS: u8 = 2;

/// What a server's start count is set to when its program isn't found:
/// it's not started again until the program is there.
const NOT_FOUND: u8 = u8::MAX;

/// A language server, its event-loop registrations, and what it's for.
#[derive(Debug)]
pub(super) struct ServerSlot {
    pub(super) server: LanguageServer,
    pub(super) tokens: [Token; 4],
    /// Its read budget ran out with more of its output waiting.
    unread: bool,
    /// Its read budget ran out with more of its stderr waiting.
    unread_errors: bool,
    pub(super) command: Vec<String>,
    pub(super) root: PathBuf,
    /// When its last file closed, if none is open in it now: it's stopped
    /// once it's been idle as long as the config says, so going to a
    /// library's file and back, or closing a project's last file and
    /// opening another, doesn't start it again and have it read the whole
    /// project again.
    idle_since: Option<Instant>,
    /// The files it has asked to hear of changes to, and the changes
    /// waiting to be told.
    pub(super) wanted: Wanted,
}

/// The language servers, and how often each project's has been started.
#[derive(Debug, Default)]
pub(super) struct Servers {
    pub(super) slots: std::collections::BTreeMap<ServerId, ServerSlot>,
    /// Servers stopped, on their way out.
    pub(super) stopping: BTreeMap<ServerId, Stopping>,
    /// The walks of directories to watch for servers, by their tickets.
    pub(super) walks: HashMap<u64, Walking>,
    pub(super) next_walk: u64,
    next: u32,
    started: Vec<(Vec<String>, PathBuf, u8)>,
    /// Clients' queries waiting for their servers' answers.
    asked: Vec<Asked>,
    /// Files answers have gone to, and the server that answered, which
    /// they're opened in rather than one of their own: a library's file
    /// gone to from a project is the project's server's to know.
    reached: HashMap<PathBuf, ServerId>,
    /// What servers say is wrong in files not open, by path, with the
    /// server that said it.
    pub(super) elsewhere: HashMap<PathBuf, (ServerId, std::sync::Arc<[Reported]>)>,
    /// Queries to ask again, and when: their servers said the text changed
    /// while they worked.
    again: Vec<(Instant, Again)>,
    /// The completion items last answered to each client about each
    /// buffer, as their servers sent them, to resolve one when asked.
    listed: Vec<Listed>,
    /// The code actions last answered to each client about each buffer,
    /// as their servers sent them, to do one when it's picked.
    offered: Vec<Offered>,
}

/// The code actions answered to a client's code actions query, as the
/// server sent them, in the order passed on.
#[derive(Debug)]
struct Offered {
    token: Token,
    buffer: BufferId,
    /// The client's number for the query.
    id: u64,
    server: ServerId,
    actions: Vec<Value>,
    /// Every buffer open in the server as it was when asked, which the
    /// edits the actions come with are counted in.
    before: Vec<Before>,
}

/// The completion items answered to a client's completion query, as the
/// server sent them, in the order passed on.
#[derive(Debug)]
struct Listed {
    token: Token,
    buffer: BufferId,
    /// The client's number for the query.
    id: u64,
    items: Vec<Value>,
}

/// A query to ask again.
#[derive(Debug)]
struct Again {
    token: Token,
    buffer: BufferId,
    place: (u64, u64, usize),
    ask: Ask,
    tries: u8,
}

impl Servers {
    /// Keeps what server `server` says is wrong in the file at `path`,
    /// which isn't open, in place of what it said before; or forgets it, if
    /// nothing is. Past [`MAX_ELSEWHERE`] files, more aren't kept. Returns
    /// the server whose say about the file was forgotten, if one's was,
    /// and it's no longer the one to say.
    fn elsewhere(
        &mut self,
        server: ServerId,
        path: &std::path::Path,
        mut diagnostics: Vec<Reported>,
    ) -> Option<ServerId> {
        let had = self.elsewhere.get(path).map(|(said, _)| *said);
        if diagnostics.is_empty() {
            // Another server's say about the file stays.
            if had == Some(server) {
                self.elsewhere.remove(path);
                return had;
            }
        } else if self.elsewhere.len() < MAX_ELSEWHERE || had.is_some() {
            diagnostics.truncate(MAX_ELSEWHERE_EACH);
            (self.elsewhere).insert(path.to_owned(), (server, diagnostics.into()));
            return had.filter(|had| *had != server);
        }
        None
    }

    /// What servers say is wrong in files not open, with each server's
    /// columns.
    pub(super) fn diagnosed_elsewhere(
        &self,
    ) -> Vec<(PathBuf, lsp::Encoding, std::sync::Arc<[Reported]>)> {
        (self.elsewhere.iter())
            .filter_map(|(path, (server, diagnostics))| {
                let encoding = self.slots.get(server)?.server.encoding();
                Some((path.clone(), encoding, diagnostics.clone()))
            })
            .collect()
    }

    /// Whether buffer `buffer` is being formatted.
    fn formatting(&self, buffer: BufferId) -> bool {
        let format = |ask: &Ask| matches!(ask, Ask::Query(Query::Format { .. }));
        (self.asked.iter()).any(|asked| {
            asked.buffer == buffer && matches!(asked.what, Asking::Query(Query::Format { .. }))
        }) || (self.again.iter()).any(|(_, again)| again.buffer == buffer && format(&again.ask))
    }

    /// Forgets how often servers whose program is `program` have been
    /// started, as it's just been installed.
    pub(super) fn forget_starts(&mut self, program: &str) {
        (self.started)
            .retain(|(command, _, _)| command.first().is_none_or(|first| first != program));
    }
}

/// A client's query, as asked of a server.
#[derive(Debug)]
struct Asked {
    server: ServerId,
    /// The server's number for the request.
    request: u64,
    /// The client that asked, about which buffer, and its number for it.
    token: Token,
    buffer: BufferId,
    id: u64,
    /// The buffer's version and text when the server was asked, which
    /// places in it are counted in, and the place asked about.
    version: u64,
    text: Rope,
    at: usize,
    /// What was asked.
    what: Asking,
    /// How many times it's been asked before, and said the text changed
    /// while it worked.
    tries: u8,
}

/// What a client asks a buffer's language server.
#[derive(Debug)]
enum Ask {
    /// About a place.
    Query(Query),
    /// To rename what's at a place to this.
    Rename(String),
    /// To say the rest of this code action, as sent, so as to do it, once
    /// it's been taken from the list it was answered in.
    Action(Value),
}

/// What a server was asked, as kept until it answers.
#[derive(Debug)]
enum Asking {
    Query(Query),
    /// A rename to `name`, with every buffer open in the server as it was
    /// when asked, which the edits it answers with are counted in.
    Rename {
        name: String,
        before: Vec<Before>,
    },
    /// What could be done to the text from the place to byte `end`, with
    /// every buffer open in the server as it was when asked, which the
    /// edits the actions come with are counted in.
    CodeActions {
        end: usize,
        before: Vec<Before>,
    },
    /// The rest of `action`, a code action as sent, called `title`, to do
    /// once it comes, with every buffer open in the server as it was when
    /// asked, as for a rename.
    Act {
        action: Value,
        title: String,
        before: Vec<Before>,
    },
    /// A command run, as the code action called `title` says, after its
    /// edits changed `files` files, and the server's own edits since.
    Command {
        title: String,
        files: usize,
    },
}

/// How long to wait before asking a server again what it said it couldn't
/// answer because the text changed while it worked, after each time:
/// rust-analyzer says so for as long as it's reading a project. Asked once
/// more than there are waits.
const AGAIN_AFTER: [Duration; 2] = [Duration::from_millis(300), Duration::from_secs(1)];

/// A buffer open in a server as it was when the server was asked something
/// it answers with edits, such as a rename, which are counted in it.
#[derive(Clone, Debug)]
pub(super) struct Before {
    pub(super) buffer: BufferId,
    pub(super) version: u64,
    pub(super) text: Rope,
}

/// How many files not open what's wrong in them is kept for, at most.
const MAX_ELSEWHERE: usize = 5_000;

/// How many diagnostics of each file not open are kept, at most.
const MAX_ELSEWHERE_EACH: usize = 1_000;

/// The most places of one answer passed on: far more than anyone looks
/// through, but short of reading thousands of files for one.
const MAX_LOCATIONS: usize = 100;

/// The most of what a server says is at a place that's passed on, in
/// bytes: far more than anyone reads in a panel by the cursor.
const MAX_HOVER: usize = 64 * 1024;

/// The most completion items of one answer passed on: more than anyone
/// reads, and enough that narrowing them down as more is typed rarely
/// misses one. Those left out are the server's last, as it sorts them, and
/// the answer is marked as not all, so typing more asks again.
const MAX_COMPLETIONS: usize = 5000;

/// The most of a completion item's documentation passed on, in bytes.
const MAX_DOCUMENTATION: usize = 16 * 1024;

/// The most code actions of one answer passed on: more than a menu by the
/// cursor shows, scrolled.
const MAX_ACTIONS: usize = 200;

/// A buffer as its language server knows it.
#[derive(Debug)]
pub(super) struct Opened {
    server: ServerId,
    /// The text as sent at each version the server hasn't yet said what's
    /// wrong with, oldest first: what its diagnostics' lines and columns
    /// are counted in.
    sent: VecDeque<(u64, Rope)>,
    /// What it last said is wrong, moved along with each edit since.
    pub(super) diagnostics: Diagnostics,
    /// Each of those diagnostics as the server sent it, in their order,
    /// to send back when it's asked what could be done about them. Moving
    /// them with an edit never changes their order, as every place moves
    /// the same way and ties keep theirs, so each is still at its index.
    sent_diagnostics: Vec<Value>,
}

/// The most versions of a buffer kept for a server that never says what's
/// wrong with them. Ropes share what doesn't change, so each costs little.
const KEPT_VERSIONS: usize = 256;

impl Opened {
    /// The server it's open in.
    pub(super) fn server(&self) -> ServerId {
        self.server
    }

    /// What its server last said is wrong with any of bytes `start` to
    /// `end` of its text, `text`, or about no text at a place in them, or
    /// at the place itself, if they're one, as the server sent it, moved to where it is now, in
    /// `encoding`'s columns: what the server is told is wrong there when
    /// it's asked what could be done.
    fn wrong_at(
        &self,
        text: &Rope,
        (start, end): (usize, usize),
        encoding: lsp::Encoding,
    ) -> Vec<Value> {
        (self.diagnostics.all().iter())
            .zip(&self.sent_diagnostics)
            .filter(|(diagnostic, _)| {
                // One starting where a range ends is after it, but for
                // a range that's a place.
                let before_end = diagnostic.start < end || diagnostic.start == start;
                before_end && (start < diagnostic.end || diagnostic.start >= start)
            })
            .map(|(diagnostic, sent)| {
                let place = (diagnostic.start, diagnostic.end);
                lsp::diagnostic_at(sent, text, place, encoding)
            })
            .collect()
    }

    /// The text as sent at `version`, if it's still kept.
    pub(super) fn sent_at(&self, version: u64) -> Option<&Rope> {
        (self.sent.iter()).find_map(|(sent, text)| (*sent == version).then_some(text))
    }
}

impl Daemon {
    /// Opens buffer `id` in its language's server, starting one for its
    /// project if there's none, unless it's open already or its language
    /// has no server.
    pub(super) fn open_in_server(&mut self, id: BufferId) {
        let (Some(languages), Some(slot)) = (&mut self.languages, self.buffers.get(&id)) else {
            return;
        };
        if slot.server.is_some() {
            return;
        }
        let path = slot.buffer.path().to_owned();
        let Some(spec) = languages.find(&path).and_then(|pack| pack.server.clone()) else {
            return;
        };
        // Open, it's its buffer's to say what's wrong with now.
        if let Some((said, _)) = self.servers.elsewhere.remove(&path) {
            self.unwatch_undiagnosed(said, &path);
        }
        // A file an answer went to is opened in the server that answered,
        // if it's of the file's language.
        let reached = (self.servers.reached.get(&path).copied()).filter(|server| {
            (self.servers.slots.get(server)).is_some_and(|slot| slot.command == spec.command)
        });
        let server = if let Some(server) = reached {
            server
        } else {
            let Some(root) = spec.root(&path, &self.config.home) else {
                return;
            };
            let Some(server) = self.server_for(&spec.command, root) else {
                return;
            };
            server
        };
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let (text, version) = (slot.buffer.text().clone(), slot.buffer.version());
        if let Some(running) = self.servers.slots.get_mut(&server) {
            running.idle_since = None;
            log::info!("buffer {id}: open in {}", running.command.join(" "));
            running
                .server
                .open(&path, &spec.language_id, version, &text);
        }
        slot.server = Some(Opened {
            server,
            sent: VecDeque::from([(version, text)]),
            diagnostics: Diagnostics::new(),
            sent_diagnostics: Vec::new(),
        });
        self.flush_server(server);
    }

    /// The server running `command` for the project in `root`, started if
    /// it isn't running, unless it's been started as often as it may be.
    fn server_for(&mut self, command: &[String], root: PathBuf) -> Option<ServerId> {
        let servers = &mut self.servers;
        if let Some((&id, _)) =
            (servers.slots.iter()).find(|(_, slot)| slot.command == command && slot.root == root)
        {
            return Some(id);
        }
        let index = (servers.started.iter())
            .position(|(started, at, _)| started == command && *at == root)
            .unwrap_or_else(|| {
                servers.started.push((command.to_vec(), root.clone(), 0));
                servers.started.len() - 1
            });
        let started = &mut servers.started[index].2;
        let name = command.first().map_or("language server", String::as_str);
        if *started == NOT_FOUND {
            // Looked for again, as it may have been installed meanwhile.
            let found = if name.contains('/') {
                Path::new(name).is_file()
            } else {
                match self.config.servers.as_deref() {
                    Some(servers) => clackatoo_syntax::servers::find_program(name, servers),
                    None => clackatoo_syntax::servers::on_path(name),
                }
                .is_some()
            };
            if !found {
                return None;
            }
            *started = 0;
        }
        if *started >= STARTS {
            return None;
        }
        *started += 1;
        let mut args: Vec<OsString> = command.iter().map(OsString::from).collect();
        // Not on PATH, it may be where clackatoo installed it.
        if let Some(program) = (self.config.servers.as_deref())
            .and_then(|servers| clackatoo_syntax::servers::find_program(name, servers))
        {
            args[0] = program.into_os_string();
        }
        let trace = self.config.lsp_trace.as_deref();
        let server = match LanguageServer::start(name, &args, &root, trace) {
            Ok(server) => server,
            Err(err) => {
                log::warn!("{name} couldn't start for {}: {err}", root.display());
                if err.kind() == std::io::ErrorKind::NotFound {
                    // Not installed: not started until it is.
                    *started = NOT_FOUND;
                }
                return None;
            }
        };
        let id = ServerId(servers.next);
        servers.next += 1;
        let mut tokens: Vec<Token> = Vec::new();
        let pipes = [
            (server.stdout_fd(), Pipe::Output, Interest::READABLE),
            (server.stderr_fd(), Pipe::Errors, Interest::READABLE),
            (server.stdin_fd(), Pipe::Input, Interest::WRITABLE),
            (server.exit_fd(), Pipe::Exit, Interest::READABLE),
        ];
        for (fd, pipe, interest) in pipes {
            let entry = self.sources.vacant_entry();
            let token = Token(entry.key());
            if let Err(err) = self
                .poll
                .registry()
                .register(&mut SourceFd(&fd), token, interest)
            {
                log::warn!("{name}: watching its pipes failed: {err}");
                for token in tokens {
                    self.sources.remove(token.0);
                }
                let name = format!("{}, for {}", command.join(" "), root.display());
                self.let_go(server.stop(), &name);
                return None;
            }
            entry.insert(Source::Server(id, pipe));
            tokens.push(token);
        }
        log::info!("{name} started for {}", root.display());
        self.servers.slots.insert(
            id,
            ServerSlot {
                server,
                tokens: tokens.try_into().expect("four pipes"),
                unread: false,
                unread_errors: false,
                command: command.to_vec(),
                root,
                idle_since: None,
                wanted: Wanted::default(),
            },
        );
        Some(id)
    }

    /// Tells buffer `id`'s server about `edit`, which made `version`, and
    /// moves the buffer's diagnostics along with it.
    pub(super) fn edited_in_server(&mut self, id: BufferId, version: u64, edit: &Edit) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let Some(opened) = &mut slot.server else {
            return;
        };
        opened.diagnostics.map(edit);
        let after = slot.buffer.text().clone();
        if let (Some(running), Some((_, before))) = (
            self.servers.slots.get_mut(&opened.server),
            opened.sent.back(),
        ) {
            running
                .server
                .change(slot.buffer.path(), version, edit, before, &after);
        }
        opened.sent.push_back((version, after));
        if opened.sent.len() > KEPT_VERSIONS {
            opened.sent.pop_front();
        }
        let server = opened.server;
        self.flush_server(server);
    }

    /// Tells buffer `id`'s server it's been saved.
    pub(super) fn saved_in_server(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let Some(opened) = &slot.server else { return };
        let server = opened.server;
        if let Some(running) = self.servers.slots.get_mut(&server) {
            running.server.saved(slot.buffer.path());
        }
        self.flush_server(server);
    }

    /// Tells a closed buffer's server it's closed, and stops the server if
    /// that was its last.
    pub(super) fn closed_in_server(&mut self, path: &std::path::Path, opened: Option<Opened>) {
        let Some(opened) = opened else { return };
        let still_used = (self.buffers.values()).any(|slot| {
            slot.server
                .as_ref()
                .is_some_and(|other| other.server == opened.server)
        });
        if let Some(running) = self.servers.slots.get_mut(&opened.server) {
            running.server.close(path);
            if !still_used {
                running.idle_since = Some(Instant::now());
            }
        }
        self.flush_server(opened.server);
    }

    /// When the next server to go for having no files open should go.
    pub(super) fn next_idle_stop(&self) -> Option<Instant> {
        (self.servers.slots.values())
            .filter_map(|slot| slot.idle_since)
            .min()
            .map(|since| since + self.config.server_idle)
    }

    /// Stops servers that have had no files open for as long as the config
    /// says.
    pub(super) fn stop_idle_servers(&mut self) {
        let (now, idle_for) = (Instant::now(), self.config.server_idle);
        let idle: Vec<ServerId> = (self.servers.slots.iter())
            .filter(|(_, slot)| {
                (slot.idle_since)
                    .is_some_and(|since| now.saturating_duration_since(since) >= idle_for)
            })
            .map(|(&id, _)| id)
            .collect();
        for id in idle {
            // Stopped on purpose, which doesn't count against starting it
            // again, as stopping by itself does.
            if let Some(slot) = self.servers.slots.get(&id) {
                let (command, root) = (&slot.command, &slot.root);
                (self.servers.started).retain(|(started, at, _)| started != command || at != root);
            }
            self.stop_server(id);
        }
    }

    /// The root of the project buffer `id`'s language server was started
    /// for, if it's open in one: for a library's file gone to from a
    /// project, the project's.
    pub(super) fn buffer_root(&self, id: BufferId) -> Option<PathBuf> {
        let opened = self.buffers.get(&id)?.server.as_ref()?;
        let slot = self.servers.slots.get(&opened.server)?;
        Some(slot.root.clone())
    }

    /// Forgets the queries the client at `token` asked about `buffer`, or
    /// about any buffer, as when it goes.
    pub(super) fn forget_queries(&mut self, token: Token, buffer: Option<BufferId>) {
        let forgotten = |asker: Token, about: BufferId| {
            asker == token && buffer.is_none_or(|buffer| buffer == about)
        };
        (self.servers.asked).retain(|asked| !forgotten(asked.token, asked.buffer));
        (self.servers.again).retain(|(_, again)| !forgotten(again.token, again.buffer));
        (self.servers.listed).retain(|listed| !forgotten(listed.token, listed.buffer));
        (self.servers.offered).retain(|offered| !forgotten(offered.token, offered.buffer));
        self.forget_renames(token, buffer);
        self.forget_colourings(token, buffer);
    }

    /// When the next query to ask again is due.
    pub(super) fn next_retry(&self) -> Option<Instant> {
        self.servers.again.iter().map(|(due, _)| *due).min()
    }

    /// Asks again the queries that are due.
    pub(super) fn ask_again(&mut self) {
        let now = Instant::now();
        if self.servers.again.iter().all(|(due, _)| *due > now) {
            return;
        }
        let (due, waiting) = std::mem::take(&mut self.servers.again)
            .into_iter()
            .partition(|(due, _)| *due <= now);
        self.servers.again = waiting;
        for (_, again) in due {
            let Again {
                token,
                buffer,
                place,
                ask,
                tries,
            } = again;
            self.ask_server(token, buffer, place, ask, tries);
        }
    }

    /// The directory of server `id`'s project, if it's running.
    pub(super) fn server_root(&self, id: ServerId) -> Option<PathBuf> {
        Some(self.servers.slots.get(&id)?.root.clone())
    }

    /// How server `id` counts columns, if it's running.
    pub(super) fn server_encoding(&self, id: ServerId) -> Option<lsp::Encoding> {
        Some(self.servers.slots.get(&id)?.server.encoding())
    }

    /// Handles what happened to server `id`'s pipe `pipe`.
    pub(super) fn server_event(&mut self, id: ServerId, pipe: Pipe) {
        if self.servers.stopping.contains_key(&id) {
            return self.stopping_event(id, pipe);
        }
        match pipe {
            Pipe::Output => self.read_server(id),
            Pipe::Errors => self.read_server_errors(id),
            Pipe::Input => self.flush_server(id),
            Pipe::Exit => {
                self.read_server(id);
                self.server_gone(id);
            }
        }
    }

    /// Writes what's queued for server `id`.
    pub(super) fn flush_server(&mut self, id: ServerId) {
        let Some(slot) = self.servers.slots.get_mut(&id) else {
            return;
        };
        if slot.server.has_outgoing() && slot.server.flush().is_err() {
            self.server_gone(id);
        }
    }

    /// Reads what server `id` sent, and hands it to its buffers.
    pub(super) fn read_server(&mut self, id: ServerId) {
        let Some(slot) = self.servers.slots.get_mut(&id) else {
            return;
        };
        let mut incoming = Vec::new();
        let read = slot
            .server
            .read(&mut self.buffer, OUTPUT_BUDGET, &mut incoming);
        if let Ok(more) = read {
            slot.unread = more;
        }
        for message in incoming {
            self.receive_from_server(id, message);
        }
        if read.is_err() {
            self.server_gone(id);
        } else {
            self.flush_server(id);
        }
    }

    /// Reads what server `id` wrote to stderr, for the log, as much as one
    /// read budget allows.
    fn read_server_errors(&mut self, id: ServerId) {
        if let Some(slot) = self.servers.slots.get_mut(&id) {
            slot.unread_errors = slot.server.read_errors(&mut self.buffer, OUTPUT_BUDGET);
        }
    }

    /// Reads again from servers whose read budget ran out, those stopping
    /// too.
    pub(super) fn read_servers_unread(&mut self) {
        let unread: Vec<(ServerId, bool, bool)> = (self.servers.slots.iter())
            .filter(|(_, slot)| slot.unread || slot.unread_errors)
            .map(|(&id, slot)| (id, slot.unread, slot.unread_errors))
            .collect();
        for (id, output, errors) in unread {
            if output {
                self.read_server(id);
            }
            if errors {
                self.read_server_errors(id);
            }
        }
        self.read_stopping_unread();
    }

    /// Whether a server's read budget ran out with more waiting.
    pub(super) fn servers_unread(&self) -> bool {
        (self.servers.slots.values()).any(|slot| slot.unread || slot.unread_errors)
            || self.stopping_unread()
    }

    fn receive_from_server(&mut self, server: ServerId, message: Incoming) {
        match message {
            Incoming::Diagnostics {
                path,
                version,
                diagnostics,
            } => self.diagnosed(server, &path, version, diagnostics),
            Incoming::Answer { id, result } => self.answered(server, id, result),
            Incoming::Outdated { id } => self.outdated(server, id),
            Incoming::Ready => self.server_ready(server),
            Incoming::ApplyEdit { id, edit } => self.apply_edit(server, id, &edit),
            Incoming::Watch { id, watchers } => self.watch_for_server(server, id, &watchers),
            Incoming::Unwatch { id } => self.unwatch_for_server(server, &id),
            // Asked only of servers stopping, which aren't here.
            Incoming::ShutDown => {}
        }
    }

    /// Server `server` is ready: every client following one of its
    /// buffers is told what it does as you type.
    fn server_ready(&mut self, server: ServerId) {
        let Some(running) = self.servers.slots.get(&server) else {
            return;
        };
        let update = BufferUpdate::Features(running.server.features());
        let buffers: Vec<BufferId> = (self.buffers.iter())
            .filter(|(_, slot)| {
                (slot.server.as_ref()).is_some_and(|opened| opened.server == server)
            })
            .map(|(&id, _)| id)
            .collect();
        for buffer in buffers {
            self.tell_viewers(buffer, None, &update);
        }
    }

    /// What buffer `id`'s server does as you type, if it's ready, for a
    /// client attaching to it.
    pub(super) fn server_features(&self, id: BufferId) -> Option<ServerFeatures> {
        let opened = self.buffers.get(&id)?.server.as_ref()?;
        let running = &self.servers.slots.get(&opened.server)?.server;
        running.ready().then(|| running.features())
    }

    /// Answers a client's
    /// [`ClientMessage::Query`](clackatoo_protocol::ClientMessage::Query)
    /// or [`ClientMessage::Rename`](clackatoo_protocol::ClientMessage::Rename)
    /// numbered `id` about byte `at` of buffer `buffer`'s text at
    /// `version`, by asking its server.
    pub(super) fn serve_query(&mut self, token: Token, message: ClientMessage) {
        let (buffer, place, ask) = match message {
            ClientMessage::Query {
                buffer,
                id,
                version,
                at,
                query,
            } => (buffer, (id, version, at), Ask::Query(query)),
            ClientMessage::Rename {
                buffer,
                id,
                version,
                at,
                name,
            } => (buffer, (id, version, at), Ask::Rename(name)),
            _ => return,
        };
        self.ask_server(token, buffer, place, ask, 0);
    }

    /// Asks buffer `buffer`'s server what the client at `token` asked, as
    /// [`serve_query`](Self::serve_query) says, having asked it `tries`
    /// times already.
    fn ask_server(
        &mut self,
        token: Token,
        buffer: BufferId,
        (id, version, at): (u64, u64, usize),
        ask: Ask,
        tries: u8,
    ) {
        let Some(slot) = self.buffers.get(&buffer) else {
            return;
        };
        if !slot.viewers.contains(&token) {
            log::debug!("a query about buffer {buffer} from a client not following it");
            return;
        }
        let now = slot.buffer.version();
        let failed = |why: &str| BufferUpdate::Answer {
            id,
            version: now,
            answer: Answer::Failed(why.to_owned()),
        };
        let server = match self.server_to_ask(buffer, &ask) {
            Ok(server) => server,
            Err(why) => return self.tell(token, buffer, failed(why)),
        };
        match ask {
            Ask::Query(Query::Act { list, item }) => {
                return self.pick_action(server, (token, buffer, id), (list, item));
            }
            Ask::Action(action) => {
                return self.take_action(server, (token, buffer, id), action, &[]);
            }
            Ask::Query(query) => self.supersede(token, buffer, query),
            Ask::Rename(_) => {}
        }
        let (Some(slot), Some(running)) = (
            self.buffers.get(&buffer),
            self.servers.slots.get_mut(&server),
        ) else {
            return;
        };
        let Some(opened) = &slot.server else {
            return;
        };
        // The place, moved along with edits made since the client's text,
        // such as an agent's. The server has every edit made.
        let Some(edits) = slot.buffer.edits_since(version) else {
            return self.tell(token, buffer, failed("the file has changed since"));
        };
        let edits: Vec<&Edit> = edits.collect();
        let text = slot.buffer.text().clone();
        let moved = |place: usize| {
            (edits.iter())
                .fold(place, |place, edit| edit.map(place, Bias::Before))
                .min(text.len_bytes())
        };
        let at = moved(at);
        let path = slot.buffer.path();
        let (request, what) = match ask {
            Ask::Action(_) => unreachable!("done above"),
            Ask::Query(query @ Query::Resolve { list, item }) => {
                let listed = &self.servers.listed;
                match resolve(&mut running.server, listed, (token, buffer), list, item) {
                    Ok(request) => (request, Asking::Query(query)),
                    Err(answer) => {
                        let update = BufferUpdate::Answer {
                            id,
                            version: now,
                            answer,
                        };
                        return self.tell(token, buffer, update);
                    }
                }
            }
            Ask::Query(Query::CodeActions { end }) => {
                let end = moved(end).max(at);
                let wrong = opened.wrong_at(&text, (at, end), running.server.encoding());
                let request = running.server.code_actions(path, &text, (at, end), &wrong);
                // As for a rename: the edits the actions come with are
                // made to the text it has now.
                let before = sent_last(&self.buffers, server);
                (request, Asking::CodeActions { end, before })
            }
            Ask::Query(query) => (
                running.server.ask(query, path, &text, at),
                Asking::Query(query),
            ),
            Ask::Rename(name) => {
                let request = running.server.rename(path, &text, at, &name);
                // The server makes the edits it answers with to the text
                // it has now, which is the text sent last, as it takes
                // what it's sent in order.
                let before = sent_last(&self.buffers, server);
                (request, Asking::Rename { name, before })
            }
        };
        self.servers.asked.push(Asked {
            server,
            request,
            token,
            buffer,
            id,
            version: now,
            text,
            at,
            what,
            tries,
        });
        self.flush_server(server);
    }

    /// Forgets the question of `query`'s kind the client at `token` asked
    /// about `buffer` before, if it's still waiting for its server or to
    /// be asked again, as `query` takes its place: a window asks what's at
    /// a place, what could be typed, and about a call afresh as the cursor
    /// moves or more is typed, and only wants the last answer. The server
    /// is told it needn't answer, and an answer that comes anyway is let
    /// go. No other client was waiting for it: each question is one
    /// client's, asked of the server on its own.
    fn supersede(&mut self, token: Token, buffer: BufferId, query: Query) {
        let same_kind = |asked: Query| {
            matches!(
                (query, asked),
                (Query::Hover, Query::Hover)
                    | (Query::Completion(_), Query::Completion(_))
                    | (Query::SignatureHelp { .. }, Query::SignatureHelp { .. })
            )
        };
        let mut cancelled = Vec::new();
        self.servers.asked.retain(|asked| {
            let replaced = asked.token == token
                && asked.buffer == buffer
                && matches!(asked.what, Asking::Query(asked) if same_kind(asked));
            if replaced {
                cancelled.push((asked.server, asked.request));
            }
            !replaced
        });
        self.servers.again.retain(|(_, again)| {
            !(again.token == token
                && again.buffer == buffer
                && matches!(again.ask, Ask::Query(asked) if same_kind(asked)))
        });
        for (server, request) in cancelled {
            if let Some(running) = self.servers.slots.get_mut(&server) {
                running.server.cancel(request);
                self.flush_server(server);
            }
        }
        // Colours still to come for a hover before are for one gone.
        if query == Query::Hover {
            self.forget_colourings(token, Some(buffer));
        }
    }

    /// The server to ask what `ask` asks about buffer `buffer`, which is
    /// open: its own, if it has one, ready, that does what's asked. Or why
    /// it's not asked.
    fn server_to_ask(&self, buffer: BufferId, ask: &Ask) -> Result<ServerId, &'static str> {
        let opened = (self.buffers.get(&buffer))
            .and_then(|slot| slot.server.as_ref())
            .ok_or("this file has no language server")?;
        // Two answers counted in one text would make the edits twice.
        if matches!(ask, Ask::Query(Query::Format { .. })) && self.servers.formatting(buffer) {
            return Err("it's being formatted already");
        }
        let running =
            (self.servers.slots.get(&opened.server)).ok_or("its language server has stopped")?;
        if !running.server.ready() {
            return Err("its language server is still starting");
        }
        let cant = match ask {
            Ask::Query(query) => (!running.server.answers(*query)).then(|| cannot(*query)),
            Ask::Rename(_) => {
                (!running.server.renames()).then_some("its language server can't rename")
            }
            Ask::Action(_) => None,
        };
        cant.map_or(Ok(opened.server), Err)
    }

    /// Asks again, after a while, what server `server` didn't answer, its
    /// request numbered `request`, because the text changed while it
    /// worked, unless it's been asked as often as it may be.
    fn outdated(&mut self, server: ServerId, request: u64) {
        let Some(index) = (self.servers.asked.iter())
            .position(|asked| asked.server == server && asked.request == request)
        else {
            return;
        };
        let asked = self.servers.asked.swap_remove(index);
        let (token, buffer, id) = (asked.token, asked.buffer, asked.id);
        let busy = "its language server is busy with changes: try again";
        let Some(wait) = AGAIN_AFTER.get(usize::from(asked.tries)) else {
            return self.answer_client((token, buffer, id), Answer::Failed(busy.into()));
        };
        let ask = match asked.what {
            Asking::Query(query) => Ask::Query(query),
            Asking::Rename { name, .. } => Ask::Rename(name),
            Asking::CodeActions { end, .. } => Ask::Query(Query::CodeActions { end }),
            Asking::Act { action, .. } => Ask::Action(action),
            // Running it again would make its action's edits twice.
            Asking::Command { .. } => {
                return self.answer_client((token, buffer, id), Answer::Failed(busy.into()));
            }
        };
        let again = Again {
            token,
            buffer,
            place: (id, asked.version, asked.at),
            ask,
            tries: asked.tries + 1,
        };
        self.servers.again.push((Instant::now() + *wait, again));
    }

    /// Forgets formatting buffer `id` asked for but not yet made, as it's
    /// being written as it is: edits made after would leave it changed
    /// again, unseen. Whoever asked is told.
    pub(super) fn forget_formats(&mut self, id: BufferId) {
        let mut told = Vec::new();
        self.servers.asked.retain(|asked| {
            let format = matches!(asked.what, Asking::Query(Query::Format { .. }));
            if asked.buffer == id && format {
                told.push((asked.token, asked.id));
            }
            !(asked.buffer == id && format)
        });
        self.servers.again.retain(|(_, again)| {
            let format = matches!(again.ask, Ask::Query(Query::Format { .. }));
            if again.buffer == id && format {
                told.push((again.token, again.place.0));
            }
            !(again.buffer == id && format)
        });
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let version = slot.buffer.version();
        for (token, query) in told {
            let update = BufferUpdate::Answer {
                id: query,
                version,
                answer: Answer::Failed("it was written before it was formatted".into()),
            };
            self.tell(token, id, update);
        }
    }

    /// Passes on the answer to the request numbered `request` to server
    /// `server`, to the client that asked, if it still follows the buffer.
    fn answered(&mut self, server: ServerId, request: u64, result: Result<Value, String>) {
        let Some(index) = (self.servers.asked.iter())
            .position(|asked| asked.server == server && asked.request == request)
        else {
            return;
        };
        let asked = self.servers.asked.swap_remove(index);
        let Some(slot) = self.buffers.get(&asked.buffer) else {
            return;
        };
        if !slot.viewers.contains(&asked.token) {
            return;
        }
        let answer = match (result, &asked.what) {
            (Err(err), _) => Answer::Failed(err),
            (Ok(value), Asking::Query(Query::Hover)) => self.hover(server, &asked, &value),
            (Ok(value), Asking::Query(Query::Format { .. })) => {
                self.formatted(server, (asked.buffer, asked.version, &asked.text), &value)
            }
            (Ok(value), Asking::Query(Query::Completion(_))) => {
                self.completions(server, &asked, value)
            }
            (Ok(value), Asking::Query(Query::Resolve { .. })) => resolved(&value),
            (Ok(value), Asking::Query(Query::SignatureHelp { .. })) => {
                let encoding = self.server_encoding(server).unwrap_or(lsp::Encoding::Utf16);
                lsp::signature(&value, encoding).map_or(Answer::Nothing, |signature| {
                    Answer::Signature {
                        label: cut(signature.label, MAX_HOVER),
                        parameter: signature.parameter,
                        index: signature.index,
                        count: signature.count,
                    }
                })
            }
            (Ok(value), Asking::Rename { before, .. }) => {
                let purpose = Purpose::Rename {
                    token: asked.token,
                    buffer: asked.buffer,
                    id: asked.id,
                };
                // Told once it's made, which may be once files not open
                // have been written.
                return self.edit_workspace(server, before, &value, purpose);
            }
            (Ok(value), Asking::CodeActions { before, .. }) => {
                self.offer_actions(server, &asked, value, before.clone())
            }
            (
                Ok(value),
                Asking::Act {
                    action,
                    title,
                    before,
                },
            ) => {
                let client = (asked.token, asked.buffer, asked.id);
                let Some(action) = lsp::resolved_action(action, value) else {
                    let why = format!("{title}: its language server couldn't say what it does");
                    return self.answer_client(client, Answer::Failed(why));
                };
                return self.do_action(server, client, &action, before);
            }
            (Ok(_), Asking::Command { title, files }) => Answer::Acted {
                title: title.clone(),
                files: *files,
            },
            (Ok(value), Asking::Query(_)) => {
                let mut read = HashMap::new();
                let found: Vec<FileLocation> = (lsp::locations(&value).into_iter())
                    .take(MAX_LOCATIONS)
                    .filter_map(|(path, position)| {
                        self.file_location(&asked, &path, position, &mut read)
                    })
                    .collect();
                // Files the server knows of, so as to open them in it,
                // not one of their own, as a library's would be.
                for place in &found {
                    self.servers
                        .reached
                        .insert(PathBuf::from(&place.path), server);
                }
                if found.is_empty() {
                    Answer::Nothing
                } else {
                    Answer::Locations(found)
                }
            }
        };
        // The code in what's at a place, to colour once it's been shown.
        let code = self.hover_code(asked.token, asked.buffer, &answer);
        // A rename's edits have been sent: the answer comes after them.
        let Some(slot) = self.buffers.get(&asked.buffer) else {
            return;
        };
        let update = BufferUpdate::Answer {
            id: asked.id,
            version: slot.buffer.version(),
            answer,
        };
        self.tell(asked.token, asked.buffer, update);
        if !code.is_empty() {
            self.colour_hover((asked.token, asked.buffer, asked.id), code);
        }
    }

    /// What server `server` answered `asked`'s question of what's at a
    /// place with, `value`, with the text it's about as it is now.
    fn hover(&self, server: ServerId, asked: &Asked, value: &Value) -> Answer {
        let Some((text, markdown)) = lsp::hover(value) else {
            return Answer::Nothing;
        };
        Answer::Hover {
            text: cut(text, MAX_HOVER),
            markdown,
            range: (lsp::hover_range(value)).and_then(|range| self.range_now(server, asked, range)),
        }
    }

    /// Bytes `start` to `end` of the buffer `asked` asked about, as server
    /// `server` counted them in the text it was asked about, as they are
    /// now, moved along with edits made since: or `None` if an edit since
    /// changed them, or put text in beside them, or they make no sense.
    fn range_now(
        &self,
        server: ServerId,
        asked: &Asked,
        (start, end): (lsp::Position, lsp::Position),
    ) -> Option<(usize, usize)> {
        let encoding = self.server_encoding(server)?;
        let start = lsp::place(&asked.text, start, encoding);
        let end = lsp::place(&asked.text, end, encoding);
        if start > end {
            return None;
        }
        let edits = self
            .buffers
            .get(&asked.buffer)?
            .buffer
            .edits_since(asked.version)?;
        (edits.into_iter()).try_fold((start, end), |(start, end), edit| {
            edit.map_untouched(start, end)
        })
    }

    /// The code actions server `server` answered with, `value`, to what
    /// `asked` asked, as many as are passed on, in the order they're to be
    /// shown. They're kept, as sent, to be done, with the buffers open in
    /// the server as they were `before` it was asked, which their edits
    /// are counted in.
    fn offer_actions(
        &mut self,
        server: ServerId,
        asked: &Asked,
        value: Value,
        before: Vec<Before>,
    ) -> Answer {
        let actions = lsp::code_actions(value);
        let (shown, raw): (Vec<_>, Vec<_>) = actions.into_iter().take(MAX_ACTIONS).unzip();
        let offered = &mut self.servers.offered;
        offered.retain(|offered| offered.token != asked.token || offered.buffer != asked.buffer);
        offered.push(Offered {
            token: asked.token,
            buffer: asked.buffer,
            id: asked.id,
            server,
            actions: raw,
            before,
        });
        Answer::CodeActions(shown)
    }

    /// Tells the client at `token` the answer to its query numbered `id`
    /// about `buffer`, if it still follows the buffer.
    pub(super) fn answer_client(
        &mut self,
        (token, buffer, id): (Token, BufferId, u64),
        answer: Answer,
    ) {
        let Some(slot) = self.buffers.get(&buffer) else {
            return;
        };
        if !slot.viewers.contains(&token) {
            return;
        }
        let update = BufferUpdate::Answer {
            id,
            version: slot.buffer.version(),
            answer,
        };
        self.tell(token, buffer, update);
    }

    /// Does action `item` of the code actions server `server` answered
    /// the client's query numbered `list` with, for the client's query
    /// numbered `id`. The list is used up: doing one of them again would
    /// make edits that only put text in, such as an import, twice.
    fn pick_action(
        &mut self,
        server: ServerId,
        client: (Token, BufferId, u64),
        (list, item): (u64, u32),
    ) {
        let (token, buffer, _) = client;
        let offered = &mut self.servers.offered;
        let found = (offered.iter())
            .position(|offered| offered.token == token && offered.buffer == buffer)
            .filter(|&index| offered[index].id == list && offered[index].server == server)
            .map(|index| offered.swap_remove(index))
            .and_then(|mut offered| {
                let index = usize::try_from(item).ok()?;
                (index < offered.actions.len())
                    .then(|| (offered.actions.swap_remove(index), offered.before))
            });
        let Some((action, before)) = found else {
            let why = "those actions are out of date: ask again";
            return self.answer_client(client, Answer::Failed(why.into()));
        };
        self.take_action(server, client, action, &before);
    }

    /// Does `action`, a code action or a command server `server` sent, for
    /// the client's query numbered `id`: as it comes, counted in the
    /// buffers open in the server as they were `before`, or once the
    /// server has said the rest of it, if it came without its edits.
    fn take_action(
        &mut self,
        server: ServerId,
        client: (Token, BufferId, u64),
        action: Value,
        before: &[Before],
    ) {
        let (token, buffer, id) = client;
        let title = lsp::action_title(&action);
        if lsp::is_command(&action) {
            return self.run_command(server, client, title, &action, 0);
        }
        let Some(running) = self.servers.slots.get_mut(&server) else {
            let why = "its language server has stopped";
            return self.answer_client(client, Answer::Failed(why.into()));
        };
        let complete = action.get("edit").is_some_and(|edit| !edit.is_null());
        let disabled = lsp::code_action(&action).is_none_or(|action| action.disabled.is_some());
        if complete || disabled || !running.server.resolves_actions() {
            return self.do_action(server, client, &action, before);
        }
        // Its edits are worked out only now, counted in the text the
        // server has now, as a rename's are.
        let request = running.server.resolve_action(&action);
        let before = sent_last(&self.buffers, server);
        let Some(slot) = self.buffers.get(&buffer) else {
            return;
        };
        self.servers.asked.push(Asked {
            server,
            request,
            token,
            buffer,
            id,
            version: slot.buffer.version(),
            text: slot.buffer.text().clone(),
            at: 0,
            what: Asking::Act {
                action,
                title,
                before,
            },
            tries: 0,
        });
        self.flush_server(server);
    }

    /// Does `action`, a code action server `server` sent, whole, for the
    /// client's query numbered `id`: makes its edits, counted in the
    /// buffers open in the server as they were `before`, then runs its
    /// command. The client is told once both are done, or why neither
    /// will be: the server says it can't be done now, or it does nothing.
    fn do_action(
        &mut self,
        server: ServerId,
        (token, buffer, id): (Token, BufferId, u64),
        action: &Value,
        before: &[Before],
    ) {
        let title = lsp::action_title(action);
        if let Some(why) = lsp::code_action(action).and_then(|action| action.disabled) {
            return self.answer_client((token, buffer, id), Answer::Failed(why));
        }
        let command = (action.get("command"))
            .filter(|command| command.is_object())
            .cloned();
        let edit = action.get("edit").unwrap_or(&Value::Null);
        if edit.is_null() {
            let Some(command) = command else {
                let why = format!("{title}: its language server gave it nothing to do");
                return self.answer_client((token, buffer, id), Answer::Failed(why));
            };
            return self.run_command(server, (token, buffer, id), title, &command, 0);
        }
        let purpose = Purpose::Action {
            token,
            buffer,
            id,
            title,
            command,
        };
        self.edit_workspace(server, before, edit, purpose);
    }

    /// Asks server `server` to run `command`, an LSP `Command`, as the code
    /// action called `title` says, its edits having changed `files` files,
    /// for the client's query numbered `id`. The client is told once it has
    /// run, or that its edits were made, if the command is one only the
    /// server's own editor knows.
    pub(super) fn run_command(
        &mut self,
        server: ServerId,
        client: (Token, BufferId, u64),
        title: String,
        command: &Value,
        files: usize,
    ) {
        let (token, buffer, id) = client;
        // One at a time, so the edits the server asks for while it runs
        // are the one command's.
        if self.running_command(server) {
            let why = format!(
                "{title}: its language server is running another command: try again once it's done"
            );
            return self.answer_client(client, Answer::Failed(why));
        }
        let Some(running) = self.servers.slots.get_mut(&server) else {
            let why = "its language server has stopped";
            return self.answer_client(client, Answer::Failed(why.into()));
        };
        let name = (command.get("command").and_then(Value::as_str)).unwrap_or_default();
        if !running.server.runs(name) {
            // Such as one that moves VS Code's cursor after an edit, which
            // has been made.
            log::info!("{title}: {name} is its editor's command, not run");
            if files > 0 {
                return self.answer_client(client, Answer::Acted { title, files });
            }
            let why =
                format!("{title}: {name} is a command for its editor, which clackatoo can't run");
            return self.answer_client(client, Answer::Failed(why));
        }
        let request = running.server.execute(command);
        let Some(slot) = self.buffers.get(&buffer) else {
            return;
        };
        self.servers.asked.push(Asked {
            server,
            request,
            token,
            buffer,
            id,
            version: slot.buffer.version(),
            text: slot.buffer.text().clone(),
            at: 0,
            what: Asking::Command { title, files },
            tries: 0,
        });
        self.flush_server(server);
    }

    /// Whether server `server` is running a command asked of it.
    fn running_command(&self, server: ServerId) -> bool {
        (self.servers.asked.iter())
            .any(|asked| asked.server == server && matches!(asked.what, Asking::Command { .. }))
    }

    /// Makes the edit server `server` asks for in its request numbered
    /// `request`, `edit`, a `WorkspaceEdit`, if it's running a command,
    /// which it's making it for. One asked for unasked is refused, so a
    /// server can't write files, or change buffers no one is looking at,
    /// by itself. Some servers do ask unasked, such as after a file is
    /// renamed, to fix what refers to it; clackatoo refuses those for now.
    fn apply_edit(&mut self, server: ServerId, request: Value, edit: &Value) {
        if !self.running_command(server) {
            let why = "clackatoo makes a language server's edits only while it runs a command \
                       asked of it";
            return self.answer_apply_edit(server, &request, Err(why.into()));
        }
        // The server made it to the text it has, which is the text sent
        // last, as it takes what it's sent in order.
        let before = sent_last(&self.buffers, server);
        let purpose = Purpose::Asked { request };
        self.edit_workspace(server, &before, edit, purpose);
    }

    /// Answers server `server`'s request numbered `request` to make an edit
    /// across files, which went as `made` says. Files it changed count for
    /// the command it's running, which it's making it for.
    pub(super) fn answer_apply_edit(
        &mut self,
        server: ServerId,
        request: &Value,
        made: Result<Made, String>,
    ) {
        let Some(running) = self.servers.slots.get_mut(&server) else {
            return;
        };
        let result = match made {
            Ok(made) => {
                // There's one at a time.
                let command = (self.servers.asked.iter_mut())
                    .filter(|asked| asked.server == server)
                    .find_map(|asked| match &mut asked.what {
                        Asking::Command { files, .. } => Some(files),
                        _ => None,
                    });
                if let Some(files) = command {
                    *files += made.files;
                }
                json!({ "applied": true })
            }
            Err(why) => {
                log::info!("{}: its edit wasn't made: {why}", running.command.join(" "));
                json!({ "applied": false, "failureReason": why })
            }
        };
        running.server.answer_request(request, &result);
        self.flush_server(server);
    }

    /// The completion items server `server` answered with, `value`, to
    /// what `asked` asked: their places moved along with edits made while
    /// it thought, as many as are passed on. They're kept, as sent, to be
    /// resolved.
    fn completions(&mut self, server: ServerId, asked: &Asked, value: Value) -> Answer {
        let (Some(encoding), Some(slot)) = (
            self.server_encoding(server),
            self.buffers.get(&asked.buffer),
        ) else {
            return Answer::Failed("its language server has stopped".into());
        };
        let Some(edits) = slot.buffer.edits_since(asked.version) else {
            return Answer::Failed("the file has changed since".into());
        };
        let edits: Vec<&Edit> = edits.collect();
        let len = slot.buffer.text().len_bytes();
        let now = |position| {
            let byte = lsp::place(&asked.text, position, encoding);
            (edits.iter())
                .fold(byte, |byte, edit| edit.map(byte, Bias::Before))
                .min(len)
        };
        let (raw, mut incomplete) = lsp::completion_items(value);
        let mut read: Vec<(lsp::Completion, Value)> = (raw.into_iter())
            .filter_map(|item| Some((lsp::completion(&item)?, item)))
            .collect();
        if read.len() > MAX_COMPLETIONS {
            read.sort_by_cached_key(|(item, _)| {
                item.sort.clone().unwrap_or_else(|| item.label.clone())
            });
            read.truncate(MAX_COMPLETIONS);
            incomplete = true;
        }
        let mut items = Vec::with_capacity(read.len());
        let mut kept = Vec::with_capacity(read.len());
        for (item, raw) in read {
            let more = (item.more.into_iter())
                .map(|edit| {
                    let start = now(edit.start);
                    TextChange {
                        start,
                        end: now(edit.end).max(start),
                        text: edit.text,
                    }
                })
                .collect();
            items.push(CompletionItem {
                label: item.label,
                kind: item.kind,
                detail: item.detail,
                filter: item.filter,
                sort: item.sort,
                preselect: item.preselect,
                start: item.start.map(now),
                text: item.text,
                snippet: item.snippet,
                more,
            });
            kept.push(raw);
        }
        let listed = &mut self.servers.listed;
        listed.retain(|listed| listed.token != asked.token || listed.buffer != asked.buffer);
        listed.push(Listed {
            token: asked.token,
            buffer: asked.buffer,
            id: asked.id,
            items: kept,
        });
        Answer::Completions { items, incomplete }
    }

    /// `position`, in the file at `path` as server `server` counts it, as a
    /// line and a column in bytes. In the buffer asked about, it's counted
    /// in the text the server was asked about, and moved along with edits
    /// made since; in another buffer open in the server, in its text; and
    /// otherwise in the file on disk, read once into `read`.
    fn file_location(
        &self,
        asked: &Asked,
        path: &Path,
        position: lsp::Position,
        read: &mut HashMap<PathBuf, Option<Rope>>,
    ) -> Option<FileLocation> {
        let encoding = self.servers.slots.get(&asked.server)?.server.encoding();
        // As buffers' paths are.
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let at = |text: &Rope| {
            let byte = lsp::place(text, position, encoding);
            let line = text.byte_to_line(byte);
            (byte, line as u64, byte - text.line_to_byte(line))
        };
        let location = |text: &Rope, line, col, offset| FileLocation {
            path: path.display().to_string(),
            line,
            col,
            offset,
            text: line_text(text, line),
        };
        let asked_slot = self.buffers.get(&asked.buffer)?;
        if asked_slot.buffer.path() == path {
            let (byte, ..) = at(&asked.text);
            let edits = asked_slot.buffer.edits_since(asked.version)?;
            let byte = edits.fold(byte, |byte, edit| edit.map(byte, Bias::Before));
            let text = asked_slot.buffer.text();
            let line = text.byte_to_line(byte.min(text.len_bytes()));
            let col = byte - text.line_to_byte(line);
            return Some(location(text, line as u64, col, Some(byte)));
        }
        let open = (self.buffers.values())
            .find(|slot| slot.buffer.path() == path)
            .map(|slot| slot.buffer.text().clone());
        let text = match open {
            Some(text) => text,
            None => read
                .entry(path.clone())
                .or_insert_with(|| {
                    let bytes = std::fs::read(&path).ok()?;
                    Some(Rope::from_str(&String::from_utf8_lossy(&bytes)))
                })
                .clone()?,
        };
        let (_, line, col) = at(&text);
        Some(location(&text, line, col, None))
    }

    pub(super) fn diagnosed(
        &mut self,
        server: ServerId,
        path: &std::path::Path,
        version: Option<u64>,
        diagnostics: Vec<Reported>,
    ) {
        let Some((&id, slot)) = (self.buffers.iter_mut()).find(|(_, slot)| {
            slot.buffer.path() == path
                && slot
                    .server
                    .as_ref()
                    .is_some_and(|opened| opened.server == server)
        }) else {
            // A file not open, kept as it's said, to list, and watched,
            // for a server that watches nothing itself.
            let any = !diagnostics.is_empty();
            if let Some(forgotten) = self.servers.elsewhere(server, path, diagnostics) {
                self.unwatch_undiagnosed(forgotten, path);
            }
            if any {
                self.watch_diagnosed(server, path);
            }
            return;
        };
        let Some(running) = self.servers.slots.get(&server) else {
            return;
        };
        let encoding = running.server.encoding();
        let opened = slot.server.as_mut().expect("found by its server");
        // The text they're counted in: the version they say, or else the
        // last one sent. They're out of date if they say one older than
        // any kept, which an edit since has replaced.
        let kept = match version {
            Some(version) => (opened.sent.iter())
                .find(|(sent, _)| *sent == version)
                .or_else(|| opened.sent.back().filter(|(newest, _)| version > *newest)),
            None => opened.sent.back(),
        };
        let Some((version, text)) = kept.map(|(version, text)| (*version, text)) else {
            return;
        };
        let mut found: Vec<(Diagnostic, Value)> = (diagnostics.into_iter())
            .map(|mut reported| {
                let sent = std::mem::take(&mut reported.raw);
                (lsp::diagnostic(text, reported, encoding), sent)
            })
            .collect();
        // In the order they're kept in, as each is sent back by its index.
        found.sort_by_key(|(diagnostic, _)| diagnostic.start);
        let (found, sent): (Vec<Diagnostic>, Vec<Value>) = found.into_iter().unzip();
        let mut found = Diagnostics::of(found);
        // Edits made since the server had that text move them on, as they
        // moved the text.
        let Some(edits) = slot.buffer.edits_since(version) else {
            return;
        };
        for edit in edits {
            found.map(edit);
        }
        // Older texts won't be reported on now.
        while opened.sent.front().is_some_and(|(sent, _)| *sent < version) {
            opened.sent.pop_front();
        }
        opened.diagnostics = found;
        opened.sent_diagnostics = sent;
        let update = BufferUpdate::Diagnostics {
            version: slot.buffer.version(),
            diagnostics: opened.diagnostics.all().to_vec(),
        };
        self.tell_viewers(id, None, &update);
    }

    /// Takes server `id` out of those running, forgetting what it was
    /// asked, what it said and the files watched for it. Returns it, to be
    /// stopped or let go.
    pub(super) fn forget_server(&mut self, id: ServerId) -> Option<ServerSlot> {
        let slot = self.servers.slots.remove(&id)?;
        self.servers.asked.retain(|asked| asked.server != id);
        self.servers.reached.retain(|_, server| *server != id);
        // The edits it asked for itself are its no more.
        self.forget_asked_edits(id);
        self.servers
            .elsewhere
            .retain(|_, (server, _)| *server != id);
        // Its code actions can't be done now.
        self.servers.offered.retain(|offered| offered.server != id);
        self.unwatch_files_for(id);
        Some(slot)
    }

    /// Lets server `id` go, as it has gone by itself, and watches its
    /// process until it exits, if it hasn't.
    fn let_server_go(&mut self, id: ServerId) {
        let Some(slot) = self.forget_server(id) else {
            return;
        };
        for token in slot.tokens {
            self.sources.remove(token.0);
        }
        // Its pidfd is registered under a token just freed: it's watched
        // anew, as an orphan's.
        let process = slot.server.stop();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&process);
        if let Err(err) = self.poll.registry().deregister(&mut SourceFd(&fd)) {
            log::warn!("language server: unwatching its process failed: {err}");
        }
        let name = format!("{}, for {}", slot.command.join(" "), slot.root.display());
        self.let_go(process, &name);
    }

    /// Watches `process`, server `name`'s, which has gone or is going by
    /// itself, until it exits, to reap it. What it started is left
    /// [`EXIT_WAIT`] to go too, and then its process group is ended (see
    /// [`super::groups`]).
    fn let_go(&mut self, process: Process, name: &str) {
        match process.group() {
            Ok(group) => self.groups.end(group, name, End::Gently(EXIT_WAIT)),
            Err(err) => log::warn!("{name}: what it started can't be stopped: {err}"),
        }
        self.watch_orphan(Orphan::new(process));
    }

    /// Stops every server, as before an upgrade: each is asked to shut
    /// down, and goes on its way out (see [`super::stopping`]).
    pub(super) fn stop_servers(&mut self) {
        let ids: Vec<ServerId> = self.servers.slots.keys().copied().collect();
        for id in ids {
            self.stop_server(id);
        }
    }

    /// Server `id` has gone by itself: its buffers lose their diagnostics,
    /// and it's started again for those shown, if it may be. Those not
    /// shown are opened in it again when next attached to. The last lines
    /// it wrote to stderr are logged, as they may say why.
    pub(super) fn server_gone(&mut self, id: ServerId) {
        let Some(slot) = self.servers.slots.get_mut(&id) else {
            return;
        };
        slot.server.read_errors(&mut self.buffer, OUTPUT_BUDGET);
        let last = (slot.server.last_errors()).map_or_else(String::new, |last| format!(": {last}"));
        log::warn!(
            "{} stopped, for {}{last}",
            slot.command.join(" "),
            slot.root.display()
        );
        // What it was asked, it won't answer now.
        let unanswered: Vec<(Token, BufferId, u64)> = (self.servers.asked.iter())
            .filter(|asked| asked.server == id)
            .map(|asked| (asked.token, asked.buffer, asked.id))
            .collect();
        self.let_server_go(id);
        for (token, buffer, id) in unanswered {
            let Some(slot) = self.buffers.get(&buffer) else {
                continue;
            };
            let update = BufferUpdate::Answer {
                id,
                version: slot.buffer.version(),
                answer: Answer::Failed("its language server stopped".into()),
            };
            self.tell(token, buffer, update);
        }
        // Its completion items can't be resolved now.
        let buffers = &self.buffers;
        (self.servers.listed).retain(|listed| {
            (buffers.get(&listed.buffer))
                .and_then(|slot| slot.server.as_ref())
                .is_none_or(|opened| opened.server != id)
        });
        let cleared: Vec<BufferId> = (self.buffers.iter_mut())
            .filter(|(_, slot)| {
                slot.server
                    .as_ref()
                    .is_some_and(|opened| opened.server == id)
            })
            .map(|(&buffer, slot)| {
                slot.server = None;
                buffer
            })
            .collect();
        for buffer in cleared {
            let slot = &self.buffers[&buffer];
            let shown = !slot.viewers.is_empty();
            let update = BufferUpdate::Diagnostics {
                version: slot.buffer.version(),
                diagnostics: Vec::new(),
            };
            self.tell_viewers(buffer, None, &update);
            let none = BufferUpdate::Features(ServerFeatures::default());
            self.tell_viewers(buffer, None, &none);
            // Started again for what's shown, if it may be, as attaching
            // would start it.
            if shown {
                self.open_in_server(buffer);
            }
        }
    }
}

/// The most characters of a place's line sent with it.
const MAX_LINE_TEXT: usize = 200;

/// Line `line` of `text`, without its end, cut short if it's long.
fn line_text(text: &Rope, line: u64) -> String {
    let Some(line) = usize::try_from(line)
        .ok()
        .and_then(|line| text.get_line(line))
    else {
        return String::new();
    };
    (line.chars())
        .take_while(|&ch| ch != '\n' && ch != '\r')
        .take(MAX_LINE_TEXT)
        .collect()
}

/// Every buffer open in server `server`, among `buffers`, as last sent to
/// it, which is the text it has, as it takes what it's sent in order.
fn sent_last(buffers: &BTreeMap<BufferId, BufferSlot>, server: ServerId) -> Vec<Before> {
    (buffers.iter())
        .filter_map(|(&buffer, slot)| {
            let opened = slot.server.as_ref()?;
            let (version, text) = opened.sent.back()?;
            (opened.server == server).then(|| Before {
                buffer,
                version: *version,
                text: text.clone(),
            })
        })
        .collect()
}

/// Asks `server` to resolve item `item` of the completions answered to
/// the client at `token` about `buffer` for its query numbered `list`,
/// returning the request's number. Or the answer, if there's no need to
/// ask: there's no such item, or the server said all it will with it.
fn resolve(
    server: &mut LanguageServer,
    listed: &[Listed],
    (token, buffer): (Token, BufferId),
    list: u64,
    item: u32,
) -> Result<u64, Answer> {
    let raw = (listed.iter())
        .find(|listed| listed.token == token && listed.buffer == buffer)
        .filter(|listed| listed.id == list)
        .and_then(|listed| listed.items.get(usize::try_from(item).ok()?))
        .ok_or(Answer::Nothing)?;
    if server.resolves() {
        Ok(server.resolve(raw))
    } else {
        Err(resolved(raw))
    }
}

/// Why a server that can't answer `query` doesn't.
fn cannot(query: Query) -> &'static str {
    match query {
        Query::Format { .. } => "its language server doesn't format files",
        Query::Completion(_) | Query::Resolve { .. } => {
            "its language server doesn't complete what's typed"
        }
        Query::SignatureHelp { .. } => "its language server doesn't help with calls",
        Query::CodeActions { .. } | Query::Act { .. } => "its language server has no code actions",
        _ => "its language server doesn't say what things are",
    }
}

/// What a completion item, as a server sent it or resolved it, says of
/// itself beyond what's in the list.
fn resolved(item: &Value) -> Answer {
    let detail = item.get("detail").and_then(Value::as_str);
    Answer::Resolved {
        detail: detail.map(|detail| cut(detail.to_owned(), MAX_DOCUMENTATION)),
        documentation: lsp::documentation(item)
            .map(|(text, markdown)| (cut(text, MAX_DOCUMENTATION), markdown)),
    }
}

/// `text`, cut to at most `max` bytes, at a character's start.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}
