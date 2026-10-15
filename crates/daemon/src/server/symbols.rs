//! Finding the symbols in a project for the finder, as `<Space>ps` does:
//! the language server of the file the pane shows is asked for those
//! whose names match what's typed, and what it answers is placed, the
//! files it names read and its columns counted in bytes, on one of the
//! finder's threads (see [`crate::find`]), never on the event loop, then
//! sent on as the finder's other searches' finds are.
//!
//! A window asks afresh as what's typed changes, stopping the search
//! before, and the server is told it needn't answer that one. A server
//! that says the text changed while it worked, as rust-analyzer does while
//! it reads a project, is asked once more, a moment later.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Content, Done, Event, Found, PaneId};
use clackatoo_text::Rope;
use mio::Token;

use super::Daemon;
use super::language::{MAX_SYMBOLS, Opened, ServerId};
use crate::find::{Job, Project};
use crate::lsp;
use crate::rpc::Heavy;

/// A client's search for symbols, asked of a server and waiting for its
/// answer.
#[derive(Debug)]
pub(super) struct Searching {
    /// The client that asked, and its number for the search.
    token: Token,
    id: u64,
    server: ServerId,
    /// The server's number for the request.
    request: u64,
    /// Where the search looks, which the files found are shown from.
    project: Project,
    /// What's asked, kept to ask again.
    query: String,
    /// Whether it's been asked again already.
    again: bool,
}

/// How long a search for symbols waits to be asked again, once its server
/// said the text changed while it worked: as long as a window waits for
/// typing to pause before it asks.
const ASK_AGAIN_AFTER: Duration = Duration::from_millis(80);

impl Daemon {
    /// Starts the search for symbols matching `query` that the client at
    /// `token` asked for, numbered `id`, from the file `pane` shows: its
    /// server is asked, and what it answers is found once it does. Fails
    /// at once if there's no server to ask.
    pub(super) fn find_symbols(
        &mut self,
        token: Token,
        id: u64,
        pane: Option<PaneId>,
        query: &str,
    ) {
        let content = pane.and_then(|pane| self.workspaces.check_pane(pane).ok());
        let Some(Content::Buffer(buffer)) = content else {
            let why = "symbols are found by a file's language server: this pane shows no file";
            return self.symbols_failed(token, id, why.into());
        };
        let server = (self.buffers.get(&buffer))
            .and_then(|slot| slot.server.as_ref())
            .map(Opened::server);
        let Some(server) = server else {
            return self.symbols_failed(token, id, "this file has no language server".into());
        };
        let project = self.project(pane);
        // One that has gone can't be asked: it would never hear.
        let running =
            (self.servers.slots.get_mut(&server)).filter(|running| !running.server.has_gone());
        let Some(running) = running else {
            let why = "its language server has stopped";
            return self.symbols_failed(token, id, why.into());
        };
        if !running.server.ready() {
            let why = "its language server is still starting";
            return self.symbols_failed(token, id, why.into());
        }
        if !running.server.finds_symbols() {
            let why = "its language server doesn't find symbols in the project";
            return self.symbols_failed(token, id, why.into());
        }
        let request = running.server.find_symbols(query);
        self.servers.symbol_searches.push(Searching {
            token,
            id,
            server,
            request,
            project,
            query: query.to_owned(),
            again: false,
        });
        self.flush_server(server);
    }

    /// Server `server` said the text changed while it worked on the search
    /// for symbols it was asked numbered `request`: it's asked once more, a
    /// moment later, or if it has been, the client is told it's busy.
    pub(super) fn symbols_outdated(&mut self, server: ServerId, request: u64) {
        let Some(index) = (self.servers.symbol_searches.iter())
            .position(|search| search.server == server && search.request == request)
        else {
            return;
        };
        let mut search = self.servers.symbol_searches.swap_remove(index);
        if search.again {
            let why = "its language server is busy with changes: try again";
            return self.symbols_failed(search.token, search.id, why.into());
        }
        search.again = true;
        let due = Instant::now() + ASK_AGAIN_AFTER;
        self.servers.symbol_retries.push((due, search));
    }

    /// When the next search for symbols is to be asked again.
    pub(super) fn next_symbols_retry(&self) -> Option<Instant> {
        self.servers
            .symbol_retries
            .iter()
            .map(|(due, _)| *due)
            .min()
    }

    /// Asks again the searches for symbols that are due, of their servers,
    /// if they're still running.
    pub(super) fn ask_symbols_again(&mut self) {
        let now = Instant::now();
        let (due, waiting) = std::mem::take(&mut self.servers.symbol_retries)
            .into_iter()
            .partition(|(due, _)| *due <= now);
        self.servers.symbol_retries = waiting;
        for (_, mut search) in due {
            let Some(running) = (self.servers.slots.get_mut(&search.server))
                .filter(|running| running.server.ready() && !running.server.has_gone())
            else {
                let why = "its language server has stopped";
                self.symbols_failed(search.token, search.id, why.into());
                continue;
            };
            search.request = running.server.find_symbols(&search.query);
            let server = search.server;
            self.servers.symbol_searches.push(search);
            self.flush_server(server);
        }
    }

    /// Whether server `server`'s request numbered `request` is a search
    /// for symbols.
    pub(super) fn searching_symbols(&self, server: ServerId, request: u64) -> bool {
        (self.servers.symbol_searches.iter())
            .any(|search| search.server == server && search.request == request)
    }

    /// Takes in server `server`'s answer to the search for symbols it was
    /// asked numbered `request`: the symbols it found, placed on one of the
    /// finder's threads, go to the client that asked; or why there are
    /// none.
    pub(super) fn found_symbols(
        &mut self,
        server: ServerId,
        request: u64,
        result: Result<Heavy, String>,
    ) {
        let Some(index) = (self.servers.symbol_searches.iter())
            .position(|search| search.server == server && search.request == request)
        else {
            return;
        };
        let Searching {
            token, id, project, ..
        } = self.servers.symbol_searches.swap_remove(index);
        let value = match result {
            Ok(value) => value,
            Err(why) => return self.symbols_failed(token, id, why),
        };
        let Some(encoding) = self.server_encoding(server) else {
            let why = "its language server has stopped";
            return self.symbols_failed(token, id, why.into());
        };
        let (symbols, more) = lsp::symbols(&value, MAX_SYMBOLS);
        // The files open, in their texts, which may not be as written.
        let paths: HashSet<&Path> = (symbols.iter())
            .filter_map(|symbol| symbol.path.as_deref())
            .collect();
        let open: Vec<(PathBuf, Rope)> = (self.buffers.values())
            .filter(|slot| paths.contains(slot.buffer.path()))
            .map(|slot| (slot.buffer.path().to_owned(), slot.buffer.text().clone()))
            .collect();
        let job = Job::Symbols {
            project,
            symbols,
            encoding,
            open,
            more,
        };
        self.start_finding(token, id, job);
    }

    /// Stops the searches for symbols that `stopped` says, waiting for
    /// their servers, which are told they needn't answer, or to be asked
    /// again.
    pub(super) fn stop_symbol_searches(&mut self, stopped: impl Fn(Token, u64) -> bool) {
        (self.servers.symbol_retries).retain(|(_, search)| !stopped(search.token, search.id));
        let mut cancelled = Vec::new();
        self.servers.symbol_searches.retain(|search| {
            let stop = stopped(search.token, search.id);
            if stop {
                cancelled.push((search.server, search.request));
            }
            !stop
        });
        for (server, request) in cancelled {
            if let Some(running) = self.servers.slots.get_mut(&server) {
                running.server.cancel(request);
                self.flush_server(server);
            }
        }
    }

    /// Ends the searches for symbols waiting for server `server`, which
    /// won't answer now, or to be asked of it again, telling each client
    /// `why`.
    pub(super) fn end_symbol_searches(&mut self, server: ServerId, why: &str) {
        let (mut ended, waiting): (Vec<_>, _) = std::mem::take(&mut self.servers.symbol_searches)
            .into_iter()
            .partition(|search| search.server == server);
        self.servers.symbol_searches = waiting;
        let (again, waiting): (Vec<_>, _) = std::mem::take(&mut self.servers.symbol_retries)
            .into_iter()
            .partition(|(_, search)| search.server == server);
        self.servers.symbol_retries = waiting;
        ended.extend(again.into_iter().map(|(_, search)| search));
        for search in ended {
            self.symbols_failed(search.token, search.id, why.to_owned());
        }
    }

    /// Tells the client at `token` its search numbered `id` found nothing,
    /// for `why`.
    fn symbols_failed(&mut self, token: Token, id: u64, why: String) {
        let found = Found::Done(Done::Failed(why));
        self.send_event(token, Event::Found { id, found });
    }
}
