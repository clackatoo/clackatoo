//! Native outline/search requests, using the same index as agent tools.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clackatoo_protocol::{Content, Done, Event, Found, FoundSymbol, PaneId, Symbol};
use mio::Token;

use super::{Daemon, MOST_ASKED, QUERY_BUDGET, QUERY_WAIT, Source, Versions};
use crate::outline::{Answer, Query, Status};
use crate::work::{Asked, Outcome, Question};

/// The windows' outline requests waiting on workers.
#[derive(Default)]
pub(super) struct Gui {
    asked: Asked<Stage, Reply>,
}

/// What a window asked for.
struct Request {
    token: Token,
    /// The window's number for its search, which results go back under.
    id: u64,
    pane: Option<PaneId>,
    query: String,
    /// The outline of the pane's file, not a search of the project.
    file: bool,
    /// The window asked again on its own, to show indexing's progress.
    refresh: bool,
}

/// How far a request has got. Finding the project's root reads the disk,
/// so it's a worker's job too, before the index can be asked.
enum Stage {
    Root(Request),
    Symbols {
        request: Request,
        /// The buffers the query read, to check it's still of them.
        versions: Versions,
    },
}

impl Stage {
    /// What the window asked for.
    fn request(&self) -> &Request {
        match self {
            Self::Root(request) | Self::Symbols { request, .. } => request,
        }
    }
}

/// A worker's answer to either stage.
enum Reply {
    Root(Result<PathBuf, String>),
    Symbols(Result<Answer, String>),
}

impl Daemon {
    /// Finds definitions for the window at `token`: those of the file in
    /// `pane` if `file`, or those in its project matching `query`.
    /// Results go back as events under `id`.
    pub(in crate::server) fn find_code_symbols(
        &mut self,
        token: Token,
        id: u64,
        pane: Option<PaneId>,
        query: &str,
        file: bool,
        refresh: bool,
    ) {
        if query.len() > 256 || self.outlines.gui.asked.len() >= MOST_ASKED {
            return self.code_failed(
                token,
                id,
                "Too many outline requests or the query is too long.",
            );
        }
        let project = self.project(pane);
        let home = self.config.home.clone();
        let request = Request {
            token,
            id,
            pane,
            query: query.into(),
            file,
            refresh,
        };
        let asked = self.outlines.gui.asked.ask(
            &mut self.work.scheduler,
            Question {
                name: "outline-root",
                scope: None,
                wait: QUERY_WAIT,
            },
            Stage::Root(request),
            move |_| Reply::Root(project.root(&home)),
        );
        if let Err(err) = asked {
            self.code_failed(token, id, &err);
        }
    }

    /// Asks the index of the project at `root` what `request` wants, now
    /// its root is known.
    fn read_code_symbols(&mut self, request: Request, root: &Path) -> Result<(), String> {
        // Automatic refreshes must not restart indexing the user cancelled.
        let paused = request.refresh
            && self
                .outlines
                .projects
                .iter()
                .any(|entry| entry.project.root == root && entry.paused);
        let project = self.outline_project(root)?;
        if paused && let Some(entry) = self.outlines.projects.back_mut() {
            entry.paused = true;
        }
        project.notifications();
        let target = self.code_target(&request, root)?;
        let overlays = self.outline_overlays(root, target.as_deref());
        let versions = self.outline_versions(root, &overlays, target.as_deref());
        let query = target.map_or_else(
            || Query::Symbols {
                query: request.query.clone(),
                limit: 200,
            },
            |path| Query::File { path, limit: 1024 },
        );
        self.send_event(
            request.token,
            Event::Found {
                id: request.id,
                found: crate::find::root_found(root, &self.config.home),
            },
        );
        self.outlines.gui.asked.ask(
            &mut self.work.scheduler,
            Question {
                name: "outline-ui",
                scope: Some(root.to_string_lossy().into_owned()),
                wait: QUERY_WAIT,
            },
            Stage::Symbols { request, versions },
            move |context| {
                let cancelled = || context.cancelled();
                let answer =
                    crate::outline::collect(&project, &query, &overlays, QUERY_BUDGET, &cancelled);
                Reply::Symbols(answer.map(|mut answer| {
                    answer.status.scanning |= !paused && project.needs_work();
                    answer
                }))
            },
        )
    }

    /// The file `request` wants the outline of, as the project at `root`
    /// names it: that of its pane's buffer. `None` for a project search.
    fn code_target(&self, request: &Request, root: &Path) -> Result<Option<String>, String> {
        if !request.file {
            return Ok(None);
        }
        let buffer = request
            .pane
            .and_then(|pane| self.workspaces.check_pane(pane).ok());
        let Some(Content::Buffer(buffer)) = buffer else {
            return Err("Open a source file to show its outline.".into());
        };
        let slot = self
            .buffers
            .get(&buffer)
            .ok_or("The source buffer closed.")?;
        let path = slot
            .buffer
            .path()
            .strip_prefix(root)
            .map_err(|_| "Source is outside its project.")?
            .to_str()
            .ok_or("Non-UTF8 source path.")?;
        Ok(Some(path.to_owned()))
    }

    /// Takes the workers' answers: a root found goes on to its index, and
    /// what an index found goes to its window.
    pub(super) fn take_code_symbols(&mut self) {
        let settled = self
            .outlines
            .gui
            .asked
            .take(&self.work.scheduler, Instant::now());
        for (stage, outcome) in settled {
            let (token, id) = (stage.request().token, stage.request().id);
            if !matches!(self.sources.get(token.0), Some(Source::Client(_))) {
                continue;
            }
            let failed = match (stage, outcome) {
                (Stage::Root(request), Outcome::Answered(Reply::Root(root))) => root
                    .and_then(|root| self.read_code_symbols(request, &root))
                    .err(),
                (
                    Stage::Symbols { request, versions },
                    Outcome::Answered(Reply::Symbols(found)),
                ) => match found {
                    Ok(answer) if self.outline_fresh(&versions) => {
                        self.code_answer(&request, answer);
                        None
                    }
                    Ok(_) => {
                        Some("Source changed while reading the outline; refresh to retry.".into())
                    }
                    Err(err) => Some(err),
                },
                (_, Outcome::TimedOut) => {
                    Some("The outline took too long. Refresh to try again.".into())
                }
                (_, Outcome::Lost | Outcome::Answered(_)) => {
                    Some("The outline request was stopped. Refresh to try again.".into())
                }
            };
            if let Some(why) = failed {
                self.code_failed(token, id, &why);
            }
        }
    }

    /// Sends the window that asked `request` what its index found.
    fn code_answer(&mut self, request: &Request, answer: Answer) {
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
                message: coverage_note(&answer.status),
                ends,
            },
            Found::Done(if answer.more {
                Done::CutShort
            } else {
                Done::All
            }),
        ] {
            self.send_event(
                request.token,
                Event::Found {
                    id: request.id,
                    found,
                },
            );
        }
    }

    /// Ends the window at `token`'s search `id`, saying `why` it failed.
    fn code_failed(&mut self, token: Token, id: u64, why: &str) {
        self.send_event(
            token,
            Event::Found {
                id,
                found: Found::Done(Done::Failed(why.into())),
            },
        );
    }

    /// The window at `token` no longer wants its search `id`, or with
    /// `None`, any of its searches.
    pub(in crate::server) fn stop_code_symbols(&mut self, token: Token, id: Option<u64>) {
        self.outlines
            .gui
            .asked
            .withdraw(&self.work.scheduler, |stage| {
                let request = stage.request();
                request.token == token && id.is_none_or(|id| request.id == id)
            });
    }

    /// The daemon is handing over to a newer one: every window waiting is
    /// told to ask again.
    pub(super) fn finish_code_symbols(&mut self) {
        let waiting = self
            .outlines
            .gui
            .asked
            .withdraw(&self.work.scheduler, |_| true);
        for stage in waiting {
            let request = stage.request();
            self.code_failed(
                request.token,
                request.id,
                "The daemon is upgrading; refresh after reconnecting.",
            );
        }
    }

    /// When the loop must next look, for a request's deadline.
    pub(super) fn code_symbols_due(&self) -> Option<Instant> {
        self.outlines.gui.asked.next_due()
    }
}

/// What the window is told of the index's gaps: the last scan's first
/// error, or else how many files it holds no whole outline of.
fn coverage_note(status: &Status) -> String {
    if let Some(error) = status.errors.first() {
        return error.clone();
    }
    match (status.failed, status.truncated) {
        (0, 0) => String::new(),
        (failed, 0) => format!("{failed} files couldn't be parsed."),
        (0, truncated) => format!("{truncated} files have more definitions than are kept."),
        (failed, truncated) => format!(
            "{failed} files couldn't be parsed, and {truncated} have more definitions than \
             are kept."
        ),
    }
}
