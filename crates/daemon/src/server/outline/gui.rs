//! Native outline/search requests, using the same index as agent tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Content, Done, Event, Found, FoundSymbol, PaneId, Symbol};
use mio::Token;

use super::{Daemon, Source};
use crate::outline::{Answer, Overlay, Query};
use crate::work::{Priority, Step};

type Replies = Arc<Mutex<Vec<Ready>>>;
#[derive(Default)]
pub(super) struct Gui {
    pending: BTreeMap<u64, Pending>,
    ready: Replies,
}
struct Pending {
    token: Token,
    id: u64,
    pane: Option<PaneId>,
    query: String,
    file: bool,
    refresh: bool,
    root: Option<PathBuf>,
    versions: Vec<(PathBuf, u64)>,
    work: u64,
    due: Instant,
}
enum Ready {
    Root(u64, Result<PathBuf, String>),
    Symbols(u64, Result<Answer, String>),
}

impl Daemon {
    pub(in crate::server) fn find_code_symbols(
        &mut self,
        token: Token,
        id: u64,
        pane: Option<PaneId>,
        query: &str,
        file: bool,
        refresh: bool,
    ) {
        if query.len() > 256 || self.outlines.gui.pending.len() >= 16 {
            return self.code_failed(
                token,
                id,
                "Too many outline requests or the query is too long.",
            );
        }
        let project = self.project(pane);
        let home = self.config.home.clone();
        self.outlines.next += 1;
        let ticket = self.outlines.next;
        let ready = Arc::clone(&self.outlines.gui.ready);
        let result = self.work.scheduler.submit(
            format!("outline-root:{ticket}"),
            "Finding code project",
            None,
            Priority::Interactive,
            move |_| {
                let result = project.root(&home);
                ready
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(Ready::Root(ticket, result));
                Ok(Step::Done)
            },
        );
        match result {
            Ok(work) => {
                self.outlines.gui.pending.insert(
                    ticket,
                    Pending {
                        token,
                        id,
                        pane,
                        query: query.into(),
                        file,
                        refresh,
                        root: None,
                        versions: Vec::new(),
                        work,
                        due: Instant::now() + Duration::from_secs(10),
                    },
                );
            }
            Err(err) => self.code_failed(token, id, &err),
        }
    }

    fn read_code_symbols(
        &mut self,
        ticket: u64,
        pending: &mut Pending,
        root: &Path,
    ) -> Result<(), String> {
        // Automatic refreshes must not restart indexing the user cancelled.
        let paused = pending.refresh
            && self
                .outlines
                .projects
                .iter()
                .any(|entry| entry.project.root == root && entry.paused);
        let project = self.outline_project(root)?;
        if paused
            && let Some(entry) = self
                .outlines
                .projects
                .iter_mut()
                .find(|entry| entry.project.root == root)
        {
            entry.paused = true;
        }
        project.notifications();
        let target = self.code_target(pending, root)?;
        let mut overlays = Vec::new();
        let mut bytes = 0;
        for slot in self.buffers.values() {
            let buffer = &slot.buffer;
            let Ok(path) = buffer.path().strip_prefix(root) else {
                continue;
            };
            let Some(path) = path.to_str() else { continue };
            if pending.versions.len() >= 512 {
                return Err("Too many open buffers to validate source freshness.".into());
            }
            pending
                .versions
                .push((buffer.path().to_owned(), buffer.version()));
            if !buffer.modified() || target.as_ref().is_some_and(|target| target != path) {
                continue;
            }
            bytes += buffer.text().len_bytes();
            if overlays.len() >= 32 || bytes > 16 * 1024 * 1024 {
                return Err("Save some buffers before searching this much unsaved source.".into());
            }
            overlays.push(Overlay {
                path: path.into(),
                text: buffer.text().clone(),
                version: buffer.version(),
            });
        }
        let request = target.map_or_else(
            || Query::Symbols {
                query: pending.query.clone(),
                limit: 200,
            },
            |path| Query::File { path, limit: 1024 },
        );
        self.send_event(
            pending.token,
            Event::Found {
                id: pending.id,
                found: crate::find::root_found(root, &self.config.home),
            },
        );
        let ready = Arc::clone(&self.outlines.gui.ready);
        pending.root = Some(root.to_owned());
        pending.work = self.work.scheduler.submit(
            format!("outline-ui:{ticket}"),
            "Reading code outline",
            Some(root.to_string_lossy().into_owned()),
            Priority::Interactive,
            move |context| {
                let result = crate::outline::collect(&project, &request, &overlays, context).map(
                    |mut answer| {
                        answer.status.scanning |= !paused && project.needs_work();
                        answer
                    },
                );
                ready
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(Ready::Symbols(ticket, result));
                Ok(Step::Done)
            },
        )?;
        Ok(())
    }

    fn code_target(&self, pending: &Pending, root: &Path) -> Result<Option<String>, String> {
        let target = if pending.file {
            let buffer = pending
                .pane
                .and_then(|pane| self.workspaces.check_pane(pane).ok());
            let Some(Content::Buffer(buffer)) = buffer else {
                return Err("Open a source file to show its outline.".into());
            };
            let slot = self
                .buffers
                .get(&buffer)
                .ok_or("The source buffer closed.")?;
            Some(
                slot.buffer
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| "Source is outside its project.")?
                    .to_str()
                    .ok_or("Non-UTF8 source path.")?
                    .to_owned(),
            )
        } else {
            None
        };
        Ok(target)
    }

    pub(super) fn take_code_symbols(&mut self) {
        let ready = std::mem::take(
            &mut *self
                .outlines
                .gui
                .ready
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for reply in ready {
            let ticket = match &reply {
                Ready::Root(ticket, _) | Ready::Symbols(ticket, _) => *ticket,
            };
            let Some(mut pending) = self.outlines.gui.pending.remove(&ticket) else {
                continue;
            };
            if !matches!(self.sources.get(pending.token.0), Some(Source::Client(_))) {
                continue;
            }
            if self
                .work
                .scheduler
                .state(pending.work)
                .is_some_and(|state| {
                    matches!(
                        state,
                        clackatoo_protocol::WorkState::Cancelled
                            | clackatoo_protocol::WorkState::Cancelling
                    )
                })
            {
                self.code_failed(pending.token, pending.id, "Outline request cancelled.");
                continue;
            }
            match reply {
                Ready::Root(_, result) => match result
                    .and_then(|root| self.read_code_symbols(ticket, &mut pending, &root))
                {
                    Ok(()) => {
                        self.outlines.gui.pending.insert(ticket, pending);
                    }
                    Err(err) => self.code_failed(pending.token, pending.id, &err),
                },
                Ready::Symbols(_, result) => {
                    let changed = pending.versions.iter().any(|(path, version)| {
                        !self.buffers.values().any(|slot| {
                            slot.buffer.path() == path && slot.buffer.version() == *version
                        })
                    });
                    let new = self.buffers.values().any(|slot| {
                        slot.buffer.modified()
                            && pending
                                .root
                                .as_ref()
                                .is_some_and(|root| slot.buffer.path().starts_with(root))
                            && !pending
                                .versions
                                .iter()
                                .any(|(path, _)| slot.buffer.path() == path)
                    });
                    match result {
                        Ok(answer) if !changed && !new => self.code_answer(&pending, answer),
                        Ok(_) => self.code_failed(
                            pending.token,
                            pending.id,
                            "Source changed while reading the outline; refresh to retry.",
                        ),
                        Err(err) => self.code_failed(pending.token, pending.id, &err),
                    }
                }
            }
        }
        self.expire_code_symbols();
    }

    fn expire_code_symbols(&mut self) {
        let expired: Vec<_> = self
            .outlines
            .gui
            .pending
            .iter()
            .filter_map(|(ticket, pending)| {
                (Instant::now() >= pending.due
                    || self
                        .work
                        .scheduler
                        .state(pending.work)
                        .is_some_and(|state| {
                            matches!(
                                state,
                                clackatoo_protocol::WorkState::Cancelled
                                    | clackatoo_protocol::WorkState::Failed
                            )
                        }))
                .then_some(*ticket)
            })
            .collect();
        for ticket in expired {
            let pending = self.outlines.gui.pending.remove(&ticket).expect("listed");
            let _ = self.work.scheduler.cancel(pending.work);
            self.code_failed(
                pending.token,
                pending.id,
                "Outline request cancelled or timed out.",
            );
        }
    }

    fn code_answer(&mut self, pending: &Pending, answer: Answer) {
        let ends = answer
            .symbols
            .iter()
            .map(|symbol| u64::from(symbol.end_line.saturating_sub(1)))
            .collect();
        let symbols = answer
            .symbols
            .into_iter()
            .map(|symbol| {
                let container = symbol
                    .qualified
                    .rsplit_once("::")
                    .map_or("", |(container, _)| container);
                FoundSymbol {
                    path: symbol.path,
                    symbol: Symbol {
                        name: symbol.name,
                        kind: match symbol.kind.as_str() {
                            "function" => 12,
                            "class" => 5,
                            "type" => 26,
                            "module" => 2,
                            "impl" => 11,
                            "constant" => 14,
                            _ => 0,
                        },
                        depth: u8::try_from(
                            container
                                .split("::")
                                .filter(|part| !part.is_empty())
                                .count(),
                        )
                        .unwrap_or(4),
                        container: container.into(),
                        line: u64::from(symbol.line.saturating_sub(1)),
                        col: 0,
                        offset: None,
                    },
                }
            })
            .collect();
        for found in [
            Found::Symbols(symbols),
            Found::IndexStatus {
                complete: answer.status.complete,
                scanning: answer.status.scanning,
                files: answer.status.indexed_files,
                message: answer.status.errors.first().cloned().unwrap_or_default(),
                ends,
            },
            Found::Done(if answer.more {
                Done::CutShort
            } else {
                Done::All
            }),
        ] {
            self.send_event(
                pending.token,
                Event::Found {
                    id: pending.id,
                    found,
                },
            );
        }
    }

    fn code_failed(&mut self, token: Token, id: u64, why: &str) {
        self.send_event(
            token,
            Event::Found {
                id,
                found: Found::Done(Done::Failed(why.into())),
            },
        );
    }

    pub(in crate::server) fn stop_code_symbols(&mut self, token: Token, id: Option<u64>) {
        let stopped: Vec<_> = self
            .outlines
            .gui
            .pending
            .iter()
            .filter_map(|(ticket, pending)| {
                (pending.token == token && id.is_none_or(|id| pending.id == id)).then_some(*ticket)
            })
            .collect();
        for ticket in stopped {
            let pending = self.outlines.gui.pending.remove(&ticket).expect("listed");
            let _ = self.work.scheduler.cancel(pending.work);
        }
    }

    pub(super) fn finish_code_symbols(&mut self) {
        for (_, pending) in std::mem::take(&mut self.outlines.gui.pending) {
            let _ = self.work.scheduler.cancel(pending.work);
            self.code_failed(
                pending.token,
                pending.id,
                "The daemon is upgrading; refresh after reconnecting.",
            );
        }
    }

    pub(super) fn code_symbols_due(&self) -> Option<Instant> {
        self.outlines
            .gui
            .pending
            .values()
            .map(|pending| pending.due)
            .min()
    }
}
