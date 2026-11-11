//! Bounded index queries and ephemeral editor overlays.

use super::store::{self, Store, Symbol};
use super::{Project, Status, excluded, index_file, relative, with_parser};
use crate::work::Context;
use clackatoo_syntax::Languages;
use std::path::Path;
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

#[derive(Clone)]
pub(crate) struct Overlay {
    pub path: String,
    pub text: clackatoo_text::Rope,
    pub version: u64,
}

pub(crate) fn query(
    project: &Project,
    request: &Query,
    overlays: &[Overlay],
    context: &Context,
) -> Result<String, String> {
    let answer = collect(project, request, overlays, context)?;
    render(project, request, answer, overlays)
}

pub(crate) fn collect(
    project: &Project,
    request: &Query,
    overlays: &[Overlay],
    context: &Context,
) -> Result<Answer, String> {
    let mut store = Store::open(&project.database)?;
    let mut languages = Languages::new(project.languages.clone());
    let due = Instant::now() + Duration::from_secs(3);
    let cancelled = || context.cancelled() || Instant::now() >= due;
    let OverlayResult {
        symbols: live_symbols,
        more: overlay_more,
        mut imports,
    } = overlay_symbols(project, request, overlays, &mut languages, &cancelled)?;
    refresh_targets(
        project,
        request,
        overlays,
        &mut store,
        &mut languages,
        &cancelled,
        &mut imports,
    )?;
    let (terms, file, limit) = match request {
        Query::File { path, limit } => ("", Some(path.as_str()), *limit),
        Query::Symbols { query, limit } => (query.as_str(), None, *limit),
        Query::Map { query, .. } => (query.as_str(), None, 512),
    };
    let excluded: Vec<_> = overlays
        .iter()
        .map(|overlay| overlay.path.clone())
        .collect();
    let mut symbols = store.query(terms, file, limit + 1, &excluded)?;
    symbols.retain(|symbol| !overlays.iter().any(|overlay| overlay.path == symbol.path));
    symbols.extend(live_symbols);
    if let Query::Map { focus, .. } = request {
        for path in focus {
            if overlays.iter().any(|overlay| overlay.path == *path) {
                continue;
            }
            symbols.extend(store.query("", Some(path), 128, &excluded)?);
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
    let more = overlay_more || symbols.len() > limit;
    symbols.truncate(limit);
    if cancelled() {
        return Err(
            "Outline query cancelled or exceeded its time budget. Narrow the query.".into(),
        );
    }
    let mut status = project.status();
    status.indexed_files = store.file_count()?;
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

pub(crate) struct Answer {
    pub symbols: Vec<Symbol>,
    pub more: bool,
    imports: Vec<String>,
    pub status: Status,
}
struct OverlayResult {
    symbols: Vec<Symbol>,
    more: bool,
    imports: Vec<String>,
}

fn overlay_symbols(
    project: &Project,
    request: &Query,
    overlays: &[Overlay],
    languages: &mut Languages,
    cancelled: &dyn Fn() -> bool,
) -> Result<OverlayResult, String> {
    let mut live_symbols = Vec::new();
    let mut overlay_more = false;
    let requested_terms = match request {
        Query::File { .. } => "",
        Query::Symbols { query, .. } | Query::Map { query, .. } => query.as_str(),
    };
    let requested_words: Vec<_> = store::words(requested_terms).into_iter().take(8).collect();
    let mut imports = Vec::new();
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
        let Some(pack) = languages.find(Path::new(&overlay.path)) else {
            continue;
        };
        let outline = with_parser(&pack, |parser| {
            parser.extract(&pack, &overlay.text.to_string(), cancelled)
        })?;
        if outline.truncated {
            return Err("Unsaved source exceeds the 4096-definition limit.".into());
        }
        if matches!(request, Query::File { .. }) {
            imports = outline.imports;
        }
        for definition in outline.definitions {
            let symbol = Symbol {
                path: overlay.path.clone(),
                name: definition.name,
                qualified: definition.qualified,
                kind: definition.kind.into(),
                line: definition.line,
                end_line: definition.end_line,
                signature: definition.signature,
                unsaved: true,
            };
            let words = store::words(&format!(
                "{} {} {}",
                symbol.path, symbol.name, symbol.qualified
            ));
            let focused =
                matches!(request, Query::Map { focus, .. } if focus.contains(&overlay.path));
            if focused
                || requested_words
                    .iter()
                    .all(|word| words.iter().any(|candidate| candidate.starts_with(word)))
            {
                if live_symbols.len() < 1024 {
                    live_symbols.push(symbol);
                } else {
                    overlay_more = true;
                }
            }
        }
    }
    Ok(OverlayResult {
        symbols: live_symbols,
        more: overlay_more,
        imports,
    })
}

fn refresh_targets(
    project: &Project,
    request: &Query,
    overlays: &[Overlay],
    store: &mut Store,
    languages: &mut Languages,
    cancelled: &dyn Fn() -> bool,
    imports: &mut Vec<String>,
) -> Result<(), String> {
    let targets: Vec<&str> = match request {
        Query::File { path, .. } => vec![path.as_str()],
        Query::Map { focus, .. } => focus.iter().map(String::as_str).collect(),
        Query::Symbols { .. } => Vec::new(),
    };
    for path in targets {
        if !relative(path)
            || excluded(Path::new(path))
            || crate::ignores::Rules::new(&project.root, false, false)
                .ignored(&project.root.join(path), false)
        {
            if matches!(request, Query::Map { .. }) {
                continue;
            }
            return Err("Outline paths must be included source files inside this project.".into());
        }
        let Some(pack) = languages.find(Path::new(path)) else {
            if matches!(request, Query::Map { .. }) {
                continue;
            }
            return Err(format!("No installed grammar matches {path}."));
        };
        if overlays.iter().any(|overlay| overlay.path == path) {
            continue;
        }
        if let Err(err) = index_file(project, store, &pack, Path::new(path), 0, cancelled) {
            store.remove(path)?;
            return Err(err);
        }
        if matches!(request, Query::File { .. }) {
            *imports = store.imports(path)?;
        }
    }
    Ok(())
}

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
        if !status.errors.is_empty() {
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
