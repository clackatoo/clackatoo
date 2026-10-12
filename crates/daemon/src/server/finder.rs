//! The finder in the event loop: starting each search a client asks for,
//! with where it's to look, and passing what it finds on to the client,
//! until it's done, the client stops it, or the client goes. The searches
//! themselves run on threads of their own (see [`crate::find`]).
//!
//! What's found waits in its search's queue until the client has written
//! nearly everything it was sent, and goes a little at a time: a grep that
//! finds thousands of lines at once mustn't hold up answers, buffers'
//! edits, or, on the socket, terminals' frames, which wait for the client
//! to catch up. What a search stopped hadn't sent yet goes with it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{Content, Event, Found, PaneId, Search, ServerMessage, frame};
use mio::Token;

use super::{Daemon, Source};
use crate::find::{Job, Project, Running, Searches};

/// How much may be waiting to be written to a client before what searches
/// found waits instead.
const OUTGOING_ROOM: usize = 64 * 1024;

/// The searches going on, and the clients they're for.
#[derive(Debug)]
pub(super) struct Finds {
    searches: Searches,
    /// Each search going on, oldest first. Few at a time, as each client
    /// stops one before it starts the next.
    running: Vec<Finding>,
}

/// A search for a client, and what it's found that's waiting to be sent.
#[derive(Debug)]
struct Finding {
    /// The client that asked, and its number for the search.
    token: Token,
    id: u64,
    running: Running,
    waiting: VecDeque<Found>,
    /// It's done: it goes once what's waiting has been sent.
    done: bool,
}

impl Finds {
    /// No searches yet. `wake` wakes the event loop when one has found
    /// something.
    pub(super) fn new(home: &Path, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            searches: Searches::new(home, wake),
            running: Vec::new(),
        }
    }
}

impl Daemon {
    /// Starts the search the client at `token` asked for with
    /// [`ClientMessage::Find`](clackatoo_protocol::ClientMessage::Find),
    /// numbered `id`, in the project `pane` is in.
    pub(super) fn serve_find(
        &mut self,
        token: Token,
        id: u64,
        pane: Option<PaneId>,
        search: Search,
    ) {
        // Asking again under the same number stops the search before.
        self.stop_finding(token, id);
        let job = match search {
            Search::Files => Job::Files(self.project(pane)),
            Search::Grep { pattern, exact } => Job::Grep {
                project: self.project(pane),
                pattern,
                exact,
            },
            Search::Buffers => Job::Buffers {
                project: self.project(pane),
                buffers: (self.buffers.values().rev())
                    .map(|slot| (slot.buffer.path().to_owned(), slot.buffer.modified()))
                    .collect(),
            },
            Search::Diagnostics => Job::Diagnostics {
                project: self.project(pane),
                open: (self.buffers.values())
                    .filter_map(|slot| {
                        let opened = slot.server.as_ref()?;
                        let all = opened.diagnostics.all();
                        (!all.is_empty()).then(|| {
                            let path = slot.buffer.path().to_owned();
                            (path, all.to_vec(), slot.buffer.text().clone())
                        })
                    })
                    .collect(),
                elsewhere: self.servers.diagnosed_elsewhere(),
            },
            Search::Preview { path, line, lines } => {
                let path = self.config.home.join(path);
                let open = (self.buffers.values())
                    .find(|slot| slot.buffer.path() == path)
                    .map(|slot| slot.buffer.text().clone());
                Job::Preview {
                    path,
                    line,
                    lines,
                    open,
                }
            }
        };
        let running = self.finds.searches.start(job);
        self.finds.running.push(Finding {
            token,
            id,
            running,
            waiting: VecDeque::new(),
            done: false,
        });
    }

    /// Stops the search the client at `token` numbered `id`, if it's going
    /// on.
    pub(super) fn stop_finding(&mut self, token: Token, id: u64) {
        (self.finds.running).retain(|finding| (finding.token, finding.id) != (token, id));
    }

    /// Stops the searches of the client at `token`, which has gone.
    pub(super) fn forget_finds(&mut self, token: Token) {
        (self.finds.running).retain(|finding| finding.token != token);
    }

    /// Queues what searches have found since last asked, and sends what
    /// clients have room for.
    pub(super) fn take_found(&mut self) {
        let found: Vec<(u64, Found)> = self.finds.searches.take().collect();
        for (serial, found) in found {
            // A search stopped may have found more before it noticed.
            let finding =
                (self.finds.running.iter_mut()).find(|finding| finding.running.serial == serial);
            if let Some(finding) = finding {
                finding.done |= matches!(found, Found::Done(_));
                finding.waiting.push_back(found);
            }
        }
        let mut tokens: Vec<Token> = (self.finds.running.iter())
            .filter(|finding| !finding.waiting.is_empty())
            .map(|finding| finding.token)
            .collect();
        tokens.dedup();
        for token in tokens {
            self.send_found(token);
        }
    }

    /// Sends the client at `token` what its searches have found, the
    /// newest search's first, while it has room, forgetting the searches
    /// done and sent.
    fn send_found(&mut self, token: Token) {
        loop {
            let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                return;
            };
            let mut sent = false;
            while client.outgoing.len() < OUTGOING_ROOM {
                let Some(finding) = (self.finds.running.iter_mut().rev())
                    .find(|finding| finding.token == token && !finding.waiting.is_empty())
                else {
                    break;
                };
                let found = finding.waiting.pop_front().expect("something waits");
                let message = ServerMessage::Event(Event::Found {
                    id: finding.id,
                    found,
                });
                match frame::encode(&message) {
                    Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
                    Err(err) => log::error!("encoding what was found failed: {err}"),
                }
                sent = true;
            }
            (self.finds.running).retain(|finding| !finding.done || !finding.waiting.is_empty());
            if !sent {
                return;
            }
            self.flush_client(token);
            // Until the connection takes no more at once: then it says
            // when it's ready for more, and the loop comes back here.
            let drained = matches!(
                self.sources.get(token.0),
                Some(Source::Client(client)) if client.outgoing.is_empty()
            );
            if !drained {
                return;
            }
        }
    }

    /// Where a search from `pane` looks: the project of the file it shows,
    /// as its language server sees it, or else of its program's working
    /// directory, or the home directory.
    fn project(&mut self, pane: Option<PaneId>) -> Project {
        let home = &self.config.home;
        let content = pane.and_then(|pane| self.workspaces.check_pane(pane).ok());
        match content {
            Some(Content::Buffer(buffer)) => {
                let Some(slot) = self.buffers.get(&buffer) else {
                    return Project::Dir(home.clone());
                };
                if let Some(root) = self.buffer_root(buffer) {
                    return Project::Root(root);
                }
                let path = slot.buffer.path().to_owned();
                let server = (self.languages.as_mut())
                    .and_then(|languages| languages.find(&path))
                    .and_then(|pack| pack.server.clone());
                Project::File { path, server }
            }
            Some(Content::Terminal(_)) => {
                let dir = pane.and_then(|pane| self.working_directory(pane));
                Project::Dir(dir.map_or_else(|| home.clone(), PathBuf::from))
            }
            None => Project::Dir(home.clone()),
        }
    }
}
