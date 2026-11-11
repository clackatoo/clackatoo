//! Bounded context discovery on a background worker.
use super::{
    Arc, ContextCandidate, ContextKind, ContextRef, ContextReply, Duration, Instant, Path, Result,
};
pub(super) struct SearchJob {
    pub notice: String,
    pub kind: ContextKind,
    pub query: String,
    pub offset: usize,
    pub items: Vec<ContextCandidate>,
    pub project: Option<Arc<crate::outline::Project>>,
    pub overlays: Vec<crate::outline::Overlay>,
}
#[allow(
    clippy::too_many_lines,
    reason = "bounded symbol query, filesystem walk and result ranking form one search"
)]
pub(super) fn run(
    job: SearchJob,
    root: &Path,
    owner: clackatoo_protocol::MachineId,
    context: &crate::work::Context,
) -> Result<ContextReply> {
    let SearchJob {
        mut notice,
        kind,
        query,
        offset,
        mut items,
        project,
        overlays,
    } = job;
    if let Some(project) = project {
        let answer = crate::outline::collect(
            &project,
            &crate::outline::Query::Symbols {
                query: query.clone(),
                limit: 200,
            },
            &overlays,
            context,
        );
        match answer {
            Ok(answer) => {
                if !answer.status.complete || answer.more {
                    notice="Symbols have partial index coverage. File search is bounded; narrow the query when needed.".into();
                }
                for symbol in answer.symbols {
                    items.push(ContextCandidate {
                        kind: ContextKind::Symbol,
                        label: symbol.name.clone(),
                        detail: format!("{}:{}", symbol.path, symbol.line),
                        source: ContextRef::Symbol {
                            path: symbol.path,
                            name: symbol.name,
                            lines: (symbol.line, symbol.end_line),
                            signature: false,
                        },
                    });
                }
            }
            Err(why) if kind == ContextKind::Symbol => return Err(why),
            Err(why) => notice = format!("Symbols unavailable: {why}. Other sources are shown."),
        }
    }
    if matches!(
        kind,
        ContextKind::All | ContextKind::File | ContextKind::Project
    ) {
        let until = Instant::now() + Duration::from_millis(500);
        for entry in ignore::WalkBuilder::new(root)
            .hidden(false)
            .follow_links(false)
            .build()
            .take(50_000)
        {
            if context.cancelled() || Instant::now() > until {
                break;
            }
            let Ok(entry) = entry else { continue };
            let Some(ty) = entry.file_type() else {
                continue;
            };
            let Ok(relative) = entry.path().strip_prefix(root) else {
                continue;
            };
            let Some(path) = relative.to_str().filter(|p| !p.is_empty()) else {
                continue;
            };
            if path == ".git" || path.starts_with(".git/") || ty.is_symlink() {
                continue;
            }
            let (kind, source) = if ty.is_file() && kind != ContextKind::Project {
                (
                    ContextKind::File,
                    ContextRef::File {
                        path: path.into(),
                        lines: None,
                    },
                )
            } else if ty.is_dir() && kind == ContextKind::Project {
                (
                    ContextKind::Project,
                    ContextRef::Project { path: path.into() },
                )
            } else {
                continue;
            };
            if path.to_lowercase().contains(&query.to_lowercase()) {
                items.push(ContextCandidate {
                    kind,
                    label: path.into(),
                    detail: "Project source".into(),
                    source,
                });
            }
            if items.len() > offset + 200 {
                break;
            }
        }
    }
    let query = query.to_lowercase();
    items.retain(|i| {
        i.label.to_lowercase().contains(&query) || i.detail.to_lowercase().contains(&query)
    });
    items.sort_by_key(|i| (!i.label.to_lowercase().starts_with(&query), i.label.clone()));
    items.dedup_by(|a, b| a.source == b.source);
    let more = items.len() > offset + 100;
    Ok(ContextReply::Candidates {
        notice,
        items: items
            .into_iter()
            .skip(offset)
            .take(100)
            .map(|mut item| {
                item.source = ContextRef::Owned {
                    machine: owner,
                    source: Box::new(item.source),
                };
                item
            })
            .collect(),
        more,
    })
}
