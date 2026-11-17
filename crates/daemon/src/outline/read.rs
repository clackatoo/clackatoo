//! Bounded index queries and ephemeral editor overlays.

use super::store::{self, Store, Symbol};
use super::{Indexed, Project, Status, excluded, index_file, relative, with_parser};
use clackatoo_syntax::Languages;
use clackatoo_syntax::outline::{Failure, Outline};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The three read-only views share one index and one bounded execution path.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Query {
    File {
        path: String,
        limit: usize,
    },
    Symbols {
        query: String,
        limit: usize,
    },
    Map {
        query: String,
        focus: Vec<String>,
        max_bytes: usize,
    },
}

/// An unsaved buffer, which stands in for its file in a query's results.
#[derive(Clone)]
pub(crate) struct Overlay {
    /// The file, as the project names it.
    pub path: String,
    pub text: clackatoo_text::Rope,
    /// The buffer and its version, which together name this exact text.
    pub buffer: clackatoo_protocol::BufferId,
    pub version: u64,
}

/// The unsaved buffers a query reads in place of their files.
#[derive(Clone, Default)]
pub(crate) struct Overlays {
    pub list: Vec<Overlay>,
    /// There were more unsaved buffers than a query takes: the files of
    /// those left out are answered from disk, so the answer says it may
    /// not be whole.
    pub left_out: bool,
}

/// The most outlines of unsaved buffers kept between queries.
const MOST_CACHED: usize = 32;

/// What parsing one version of an unsaved buffer came to.
#[derive(Clone, Debug)]
enum Parsed {
    Outline(Arc<Outline>),
    /// It couldn't be parsed, and isn't tried again.
    Failed,
}

/// The outlines of unsaved buffers, each for one version of one buffer:
/// typing in one file doesn't make the next query parse the others again.
#[derive(Debug, Default)]
pub(super) struct OverlayCache {
    outlines: Vec<(clackatoo_protocol::BufferId, u64, Parsed)>,
}

impl OverlayCache {
    /// What parsing `overlay`'s version came to, if it was parsed before.
    fn get(&self, overlay: &Overlay) -> Option<Parsed> {
        self.outlines
            .iter()
            .find(|(buffer, version, _)| *buffer == overlay.buffer && *version == overlay.version)
            .map(|(_, _, outline)| outline.clone())
    }

    /// The versions kept, oldest first.
    #[cfg(test)]
    pub(super) fn versions(&self) -> Vec<u64> {
        self.outlines
            .iter()
            .map(|(_, version, _)| *version)
            .collect()
    }

    /// Keeps `outline` for `overlay`'s version, in place of any older
    /// version of its buffer.
    fn put(&mut self, overlay: &Overlay, outline: Parsed) {
        self.outlines
            .retain(|(buffer, _, _)| *buffer != overlay.buffer);
        if self.outlines.len() >= MOST_CACHED {
            self.outlines.remove(0);
        }
        self.outlines
            .push((overlay.buffer, overlay.version, outline));
    }
}

/// Answers `request` as text for an agent: JSON, or for a map, lines
/// within its byte budget.
pub(crate) fn query(
    project: &Project,
    request: &Query,
    overlays: &Overlays,
    budget: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    let answer = collect(project, request, overlays, budget, cancelled)?;
    render(project, request, answer, &overlays.list)
}

/// Answers `request` from the project's index, with `overlays` in place
/// of their files, within `budget`.
///
/// # Errors
/// If the index can't be read, the request names a file that isn't the
/// project's to outline, or the budget runs out.
pub(crate) fn collect(
    project: &Project,
    request: &Query,
    overlays: &Overlays,
    budget: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<Answer, String> {
    let store = Store::open(&project.database)?;
    let mut languages = Languages::new(project.languages.clone());
    let due = Instant::now() + budget;
    let cancelled = || cancelled() || Instant::now() >= due;
    let OverlayResult {
        symbols: live_symbols,
        more: overlay_more,
        mut imports,
        unparsed,
    } = overlay_symbols(project, request, &overlays.list, &mut languages, &cancelled);
    if let (Query::File { path, .. }, true) = (request, unparsed) {
        return Err(format!(
            "The unsaved text of {path} couldn't be parsed, so it has no outline."
        ));
    }
    refresh_targets(
        project,
        request,
        &overlays.list,
        &store,
        &mut languages,
        &cancelled,
        &mut imports,
    )?;
    let (terms, file, limit) = match request {
        Query::File { path, limit } => ("", Some(path.as_str()), *limit),
        Query::Symbols { query, limit } => (query.as_str(), None, *limit),
        Query::Map { query, .. } => (query.as_str(), None, 512),
    };
    // A buffer's file is left out of the index's answer, parsed or not:
    // what the index has of it is of the text on disk.
    let excluded: Vec<_> = overlays
        .list
        .iter()
        .map(|overlay| overlay.path.clone())
        .collect();
    let search = |terms: &str, file: Option<&str>, limit: usize| {
        let left = due.saturating_duration_since(Instant::now());
        store.query(terms, file, limit, &excluded, left.min(store::QUERY_BUDGET))
    };
    let mut symbols = search(terms, file, limit + 1)?;
    symbols.extend(live_symbols);
    if let Query::Map { focus, .. } = request {
        for path in focus {
            symbols.extend(search("", Some(path), 128)?);
        }
        symbols.sort_by_key(|symbol| {
            let focused = focus.contains(&symbol.path);
            let nearby = focus
                .iter()
                .any(|path| Path::new(path).parent() == Path::new(&symbol.path).parent());
            (
                !focused,
                !nearby,
                symbol.path.clone(),
                symbol.line,
                symbol.qualified.clone(),
            )
        });
    } else {
        symbols.sort_by_key(|symbol| (symbol.path.clone(), symbol.line, symbol.qualified.clone()));
    }
    symbols.dedup_by(|a, b| a.path == b.path && a.line == b.line && a.qualified == b.qualified);
    let more = overlays.left_out || overlay_more || symbols.len() > limit;
    symbols.truncate(limit);
    if cancelled() {
        return Err(
            "Outline query cancelled or exceeded its time budget. Narrow the query.".into(),
        );
    }
    let mut status = project.status();
    let held = store.coverage()?;
    status.indexed_files = held.files;
    status.truncated = held.truncated;
    status.failed = held.failed;
    if status.last_full_scan == 0
        && let Some(cached) = store
            .metadata("status")
            .and_then(|text| serde_json::from_str::<Status>(&text).ok())
    {
        status.last_full_scan = cached.last_full_scan;
        status.complete = false;
    }
    Ok(Answer {
        symbols,
        more,
        imports,
        status,
    })
}

/// What a query found.
pub(crate) struct Answer {
    pub symbols: Vec<Symbol>,
    /// There's more than was given: past the limit, in an unsaved buffer
    /// that couldn't be parsed or was left out, or cut off in a big one.
    pub more: bool,
    imports: Vec<String>,
    pub status: Status,
}

/// What the unsaved buffers gave a query.
struct OverlayResult {
    symbols: Vec<Symbol>,
    more: bool,
    imports: Vec<String>,
    /// A buffer asked for couldn't be parsed.
    unparsed: bool,
}

/// The outline of the unsaved buffer `overlay`: as kept from an earlier
/// query, if this version was parsed then, or parsed now and kept. `None`
/// if it can't be parsed.
fn overlay_outline(
    project: &Project,
    overlay: &Overlay,
    languages: &mut Languages,
    cancelled: &dyn Fn() -> bool,
) -> Option<Arc<Outline>> {
    let cache = || {
        project
            .overlays
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    match cache().get(overlay) {
        Some(Parsed::Outline(outline)) => return Some(outline),
        Some(Parsed::Failed) => return None,
        None => {}
    }
    let pack = languages.find(Path::new(&overlay.path))?;
    let parsed = with_parser(&pack, |parser| {
        Ok(parser.extract(&pack, &overlay.text.to_string(), cancelled))
    })
    .unwrap_or_else(|why| Err(Failure::Unparseable(why)));
    if matches!(parsed, Err(Failure::Stopped)) {
        // Stopped, not unparseable: the next query tries again.
        return None;
    }
    let outline = parsed.ok().map(Arc::new);
    let kept = outline.clone().map_or(Parsed::Failed, Parsed::Outline);
    cache().put(overlay, kept);
    outline
}

/// The definitions in `overlays` that `request` asks for. A buffer that
/// can't be parsed is left out, and one with more definitions than a file
/// may have gives its first: either way the answer says there's more.
fn overlay_symbols(
    project: &Project,
    request: &Query,
    overlays: &[Overlay],
    languages: &mut Languages,
    cancelled: &dyn Fn() -> bool,
) -> OverlayResult {
    let mut found = OverlayResult {
        symbols: Vec::new(),
        more: false,
        imports: Vec::new(),
        unparsed: false,
    };
    let requested_terms = match request {
        Query::File { .. } => "",
        Query::Symbols { query, .. } | Query::Map { query, .. } => query.as_str(),
    };
    let requested_words: Vec<_> = store::words(requested_terms).into_iter().take(8).collect();
    let rules = crate::ignores::Rules::new(&project.root, false, false).bounded(4096);
    for overlay in overlays {
        if excluded(Path::new(&overlay.path))
            || rules.ignored(&project.root.join(&overlay.path), false)
        {
            continue;
        }
        if let Query::File { path, .. } = request
            && overlay.path != *path
        {
            continue;
        }
        if languages.find(Path::new(&overlay.path)).is_none() {
            continue;
        }
        let Some(outline) = overlay_outline(project, overlay, languages, cancelled) else {
            found.more = true;
            found.unparsed = true;
            continue;
        };
        found.more |= outline.truncated;
        if matches!(request, Query::File { .. }) {
            found.imports.clone_from(&outline.imports);
        }
        let focused = matches!(request, Query::Map { focus, .. } if focus.contains(&overlay.path));
        for definition in &outline.definitions {
            let words = store::words(&format!(
                "{} {} {}",
                overlay.path, definition.name, definition.qualified
            ));
            let wanted = focused
                || requested_words
                    .iter()
                    .all(|word| words.iter().any(|candidate| candidate.starts_with(word)));
            if !wanted {
                continue;
            }
            if found.symbols.len() >= 1024 {
                found.more = true;
                break;
            }
            found.symbols.push(Symbol {
                path: overlay.path.clone(),
                name: definition.name.clone(),
                qualified: definition.qualified.clone(),
                kind: definition.kind.into(),
                line: definition.line,
                end_line: definition.end_line,
                signature: definition.signature.clone(),
                unsaved: true,
            });
        }
    }
    found
}

/// Reads the files `request` names into the index now, ahead of the
/// background scan, unless an unsaved buffer stands in for them.
fn refresh_targets(
    project: &Project,
    request: &Query,
    overlays: &[Overlay],
    store: &Store,
    languages: &mut Languages,
    cancelled: &dyn Fn() -> bool,
    imports: &mut Vec<String>,
) -> Result<(), String> {
    let (targets, required): (Vec<&str>, bool) = match request {
        Query::File { path, .. } => (vec![path.as_str()], true),
        Query::Map { focus, .. } => (focus.iter().map(String::as_str).collect(), false),
        Query::Symbols { .. } => (Vec::new(), false),
    };
    let rules = crate::ignores::Rules::new(&project.root, false, false);
    for path in targets {
        if !relative(path)
            || excluded(Path::new(path))
            || rules.ignored(&project.root.join(path), false)
        {
            if required {
                return Err(
                    "Outline paths must be included source files inside this project.".into(),
                );
            }
            continue;
        }
        let Some(pack) = languages.find(Path::new(path)) else {
            if required {
                return Err(format!("No installed grammar matches {path}."));
            }
            continue;
        };
        if overlays.iter().any(|overlay| overlay.path == path) {
            continue;
        }
        store.begin()?;
        let indexed = index_file(project, store, &pack, Path::new(path), 0, cancelled);
        if indexed.is_ok() {
            store.commit()?;
        } else {
            store.rollback();
        }
        if !required {
            continue;
        }
        if let Indexed::Skipped(why) = indexed? {
            return Err(why);
        }
        if store.failed(path)? {
            return Err(format!(
                "{path} couldn't be parsed, or took too long to: it has no outline."
            ));
        }
        *imports = store.imports(path)?;
    }
    Ok(())
}

/// `answer` as text for an agent: a map's lines within its byte budget,
/// or JSON cut down to the response budget.
fn render(
    project: &Project,
    request: &Query,
    answer: Answer,
    overlays: &[Overlay],
) -> Result<String, String> {
    let Answer {
        symbols,
        more,
        imports,
        status,
    } = answer;
    if let Query::Map { max_bytes, .. } = request {
        let mut text = format!(
            "Project: {}\nCoverage: {}\nIndexed files: {}; last full scan: {} (Unix seconds)\nDefinitions are a navigation aid; read implementations before editing.\n",
            project.root.display(),
            if status.complete {
                "reconciled"
            } else {
                "partial or refreshing"
            },
            status.indexed_files,
            status.last_full_scan
        );
        let mut emitted = 0;
        for symbol in &symbols {
            let line = format!(
                "{}:{}  {}{}  {}\n",
                symbol.path,
                symbol.line,
                symbol.qualified,
                if symbol.unsaved { " [unsaved]" } else { "" },
                symbol.signature
            );
            if text.len() + line.len() + 48 > *max_bytes {
                break;
            }
            text.push_str(&line);
            emitted += 1;
        }
        if more || emitted < symbols.len() {
            text.push_str("… more definitions available; narrow the query.\n");
        }
        if status.failed + status.truncated > 0 || !status.errors.is_empty() {
            text.push_str(
                "Some files could not be indexed; use search_symbols for coverage details.\n",
            );
        }
        while text.len() > *max_bytes {
            text.pop();
        }
        Ok(text)
    } else {
        let mut value = serde_json::json!({ "project":project.root, "coverage":status,
            "symbols":symbols, "more":more, "imports":imports,
            "unsaved_versions":overlays.iter().map(|overlay| (&overlay.path, overlay.version)).collect::<Vec<_>>() });
        loop {
            let text = serde_json::to_string(&value).map_err(|err| err.to_string())?;
            if text.len() <= 32000 {
                return Ok(text);
            }
            value["more"] = true.into();
            if value["symbols"]
                .as_array_mut()
                .is_none_or(|symbols| symbols.pop().is_none())
            {
                return Err("Outline metadata exceeds the response budget.".into());
            }
        }
    }
}
