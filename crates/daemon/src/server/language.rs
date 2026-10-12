//! Language servers in the event loop: starting one for each project a
//! buffer is in, keeping the buffer open in it as it's edited, and sending
//! what it says is wrong to every client following the buffer.
//!
//! One server runs for each command and project root, so every buffer in
//! a project shares one, as do languages with the same server, such as
//! JavaScript and TypeScript. A server goes when its last buffer closes,
//! and doesn't come through an upgrade: the next build starts it again
//! when a buffer of its is next attached to.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;

use clackatoo_protocol::{BufferId, BufferUpdate, Edit};
use clackatoo_text::{Diagnostic, Diagnostics, Rope};
use mio::unix::SourceFd;
use mio::{Interest, Token};

use super::{Daemon, OUTPUT_BUDGET, Orphan, Source};
use crate::lsp::{self, Incoming, LanguageServer, Reported};

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

/// A language server, its event-loop registrations, and what it's for.
#[derive(Debug)]
pub(super) struct ServerSlot {
    server: LanguageServer,
    tokens: [Token; 4],
    /// Its read budget ran out with more of its output waiting.
    unread: bool,
    command: Vec<String>,
    root: PathBuf,
}

/// The language servers, and how often each project's has been started.
#[derive(Debug, Default)]
pub(super) struct Servers {
    slots: std::collections::BTreeMap<ServerId, ServerSlot>,
    next: u32,
    started: Vec<(Vec<String>, PathBuf, u8)>,
}

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
}

/// The most versions of a buffer kept for a server that never says what's
/// wrong with them. Ropes share what doesn't change, so each costs little.
const KEPT_VERSIONS: usize = 256;

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
        let Some(root) = spec.root(&path, &self.config.home) else {
            return;
        };
        let Some(server) = self.server_for(&spec.command, root) else {
            return;
        };
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let (text, version) = (slot.buffer.text().clone(), slot.buffer.version());
        if let Some(running) = self.servers.slots.get_mut(&server) {
            log::info!("buffer {id}: open in {}", running.command.join(" "));
            running
                .server
                .open(&path, &spec.language_id, version, &text);
        }
        slot.server = Some(Opened {
            server,
            sent: VecDeque::from([(version, text)]),
            diagnostics: Diagnostics::new(),
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
        if *started >= STARTS {
            return None;
        }
        *started += 1;
        let name = command.first().map_or("language server", String::as_str);
        let args: Vec<OsString> = command.iter().map(OsString::from).collect();
        let server = match LanguageServer::start(name, &args, &root) {
            Ok(server) => server,
            Err(err) => {
                log::warn!("{name} couldn't start for {}: {err}", root.display());
                if err.kind() == std::io::ErrorKind::NotFound {
                    // Not installed: not looked for again.
                    *started = STARTS;
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
                self.watch_orphan(Orphan::new(server.stop()));
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
                command: command.to_vec(),
                root,
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
        if still_used {
            if let Some(running) = self.servers.slots.get_mut(&opened.server) {
                running.server.close(path);
            }
            self.flush_server(opened.server);
        } else {
            // Stopped on purpose, which doesn't count against starting it
            // again, as stopping by itself does.
            if let Some(running) = self.servers.slots.get(&opened.server) {
                let (command, root) = (&running.command, &running.root);
                self.servers
                    .started
                    .retain(|(started, at, _)| started != command || at != root);
            }
            self.stop_server(opened.server);
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

    /// Reads what server `id` wrote to stderr, for the log.
    pub(super) fn read_server_errors(&mut self, id: ServerId) {
        if let Some(slot) = self.servers.slots.get_mut(&id) {
            slot.server.read_errors(&mut self.buffer);
        }
    }

    /// Reads again from servers whose read budget ran out.
    pub(super) fn read_servers_unread(&mut self) {
        let unread: Vec<ServerId> = (self.servers.slots.iter())
            .filter(|(_, slot)| slot.unread)
            .map(|(&id, _)| id)
            .collect();
        for id in unread {
            self.read_server(id);
        }
    }

    /// Whether a server's read budget ran out with more waiting.
    pub(super) fn servers_unread(&self) -> bool {
        self.servers.slots.values().any(|slot| slot.unread)
    }

    fn receive_from_server(&mut self, server: ServerId, message: Incoming) {
        let Incoming::Diagnostics {
            path,
            version,
            diagnostics,
        } = message;
        let Some((&id, slot)) = (self.buffers.iter_mut()).find(|(_, slot)| {
            slot.buffer.path() == path
                && slot
                    .server
                    .as_ref()
                    .is_some_and(|opened| opened.server == server)
        }) else {
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
        let found: Vec<Diagnostic> = (diagnostics.into_iter())
            .map(|reported| diagnostic(text, reported, encoding))
            .collect();
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
        let update = BufferUpdate::Diagnostics {
            version: slot.buffer.version(),
            diagnostics: opened.diagnostics.all().to_vec(),
        };
        self.tell_viewers(id, None, &update);
    }

    /// Stops server `id`, and watches its process until it exits.
    pub(super) fn stop_server(&mut self, id: ServerId) {
        let Some(slot) = self.servers.slots.remove(&id) else {
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
        self.watch_orphan(Orphan::new(process));
    }

    /// Stops every server, as before an upgrade.
    pub(super) fn stop_servers(&mut self) {
        let ids: Vec<ServerId> = self.servers.slots.keys().copied().collect();
        for id in ids {
            self.stop_server(id);
        }
    }

    /// Server `id` has gone by itself: its buffers lose their diagnostics,
    /// and it's started again for those shown, if it may be. Those not
    /// shown are opened in it again when next attached to.
    pub(super) fn server_gone(&mut self, id: ServerId) {
        let Some(slot) = self.servers.slots.get(&id) else {
            return;
        };
        log::warn!(
            "{} stopped, for {}",
            slot.command.join(" "),
            slot.root.display()
        );
        self.stop_server(id);
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
            // Started again for what's shown, if it may be, as attaching
            // would start it.
            if shown {
                self.open_in_server(buffer);
            }
        }
    }
}

/// A diagnostic as reported, counted in `text`, as one at byte offsets.
fn diagnostic(text: &Rope, reported: Reported, encoding: lsp::Encoding) -> Diagnostic {
    let start = lsp::place(text, reported.start, encoding);
    let end = lsp::place(text, reported.end, encoding).max(start);
    Diagnostic {
        start,
        end,
        severity: reported.severity,
        message: reported.message,
        source: reported.source,
        code: reported.code,
    }
}
