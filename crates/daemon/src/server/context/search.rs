//! The search behind the context picker, on a worker: symbols from the
//! project's outline index, files and folders from a walk, and the ranking
//! of those with what the loop listed. Each part has its own limit, and
//! says in the reply's notice when it stopped short.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{ContextCandidate, ContextKind, ContextRef, ContextReply, MachineId};

use super::source::{Listed, Wanted};

/// How long the outline index is given to answer.
const SYMBOL_BUDGET: Duration = Duration::from_millis(1500);

/// How long, and through how many entries, the project is walked for
/// files.
const WALK_BUDGET: Duration = Duration::from_millis(500);
const MOST_WALKED: usize = 50_000;

/// The most symbols asked of the index, and results in one page.
const MOST_SYMBOLS: usize = 200;
const PAGE: usize = 100;

/// A search, with all a worker needs for it.
pub(super) struct SearchJob {
    pub kind: ContextKind,
    pub query: String,
    /// How many results the client already has.
    pub offset: usize,
    /// What the loop listed from memory, already matched against the query.
    pub listed: Listed,
    /// The project's outline index, if symbols are to be searched.
    pub project: Option<Arc<crate::outline::Project>>,
    /// Unsaved buffers, which stand in for their files in the index.
    pub overlays: crate::outline::Overlays,
    /// The chat's project, which files are found under.
    pub root: PathBuf,
    /// This daemon's machine, which every result is marked as belonging to.
    pub owner: MachineId,
}

/// A walk of `root` that follows no links and leaves out what its ignore
/// files say, read by the daemon's own bounded reader (see
/// [`crate::ignores`]): never by the `ignore` crate, which would read a
/// pipe or an endless file a hostile project left as `.gitignore`.
/// Hidden files are left out too if `hidden`.
pub(super) fn walk(root: &Path, depth: Option<usize>, hidden: bool) -> ignore::Walk {
    let rules = Arc::new(crate::ignores::Rules::new(root, false, hidden).bounded(4096));
    let mut walk = crate::ignores::walker(root);
    walk.follow_links(false)
        .max_depth(depth)
        .filter_entry(move |entry| rules.keeps(entry));
    walk.build()
}

/// Runs the search.
///
/// # Errors
/// If only symbols were asked for and the index can't be searched.
pub(super) fn run(job: SearchJob, cancelled: &dyn Fn() -> bool) -> super::Result<ContextReply> {
    let wanted = Wanted::new(&job.query);
    let mut notices = Vec::new();
    let mut items = job.listed.items;
    if job.listed.cut {
        notices.push("Only the most recent matches are listed. Type more to narrow them.");
    }
    if let Some(project) = &job.project {
        match symbols(project, &job.query, &job.overlays, cancelled) {
            Ok(found) => {
                if found.partial {
                    notices.push("The symbol index is still being built, so some may be missing.");
                }
                items.extend(found.items);
            }
            Err(why) if job.kind == ContextKind::Symbol => return Err(why),
            Err(_) => notices.push("Symbols couldn't be searched; other sources are shown."),
        }
    }
    if matches!(
        job.kind,
        ContextKind::All | ContextKind::File | ContextKind::Project
    ) {
        let enough = job.offset + 2 * PAGE;
        let found = files(&job.root, job.kind, &wanted, enough, MOST_WALKED, cancelled);
        if found.cut {
            notices.push("The project is too big to search whole. Type more of the path.");
        }
        items.extend(found.items);
    }
    items.retain(|item| wanted.matches(item));
    items.sort_by_cached_key(|item| (!wanted.starts(&item.label), item.label.clone()));
    items.dedup_by(|a, b| a.source == b.source);
    let more = items.len() > job.offset + PAGE;
    let owner = job.owner;
    let items = items
        .into_iter()
        .skip(job.offset)
        .take(PAGE)
        .map(|mut item| {
            item.source = ContextRef::Owned {
                machine: owner,
                source: Box::new(item.source),
            };
            item
        })
        .collect();
    Ok(ContextReply::Candidates {
        notice: notices.join(" "),
        items,
        more,
    })
}

/// What one part of a search found.
struct Found {
    items: Vec<ContextCandidate>,
    /// The index doesn't cover the whole project yet.
    partial: bool,
    /// The part stopped at a limit with more to look through.
    cut: bool,
}

/// The definitions the outline index has for `query`.
fn symbols(
    project: &crate::outline::Project,
    query: &str,
    overlays: &crate::outline::Overlays,
    cancelled: &dyn Fn() -> bool,
) -> super::Result<Found> {
    let request = crate::outline::Query::Symbols {
        query: query.to_owned(),
        limit: MOST_SYMBOLS,
    };
    let answer = crate::outline::collect(project, &request, overlays, SYMBOL_BUDGET, cancelled)?;
    let items = answer
        .symbols
        .into_iter()
        .map(|symbol| ContextCandidate {
            kind: ContextKind::Symbol,
            label: symbol.name.clone(),
            detail: format!("{}:{}", symbol.path, symbol.line),
            source: ContextRef::Symbol {
                path: symbol.path,
                name: symbol.name,
                lines: (symbol.line, symbol.end_line),
                signature: false,
            },
        })
        .collect();
    Ok(Found {
        items,
        partial: !answer.status.complete,
        cut: false,
    })
}

/// The files under `root` whose path has what's `wanted`, or the folders
/// if `kind` asks for those. The walk ends once it has `enough` for the
/// page asked for and the one after, and is cut short after `most_walked`
/// entries or [`WALK_BUDGET`].
fn files(
    root: &Path,
    kind: ContextKind,
    wanted: &Wanted,
    enough: usize,
    most_walked: usize,
    cancelled: &dyn Fn() -> bool,
) -> Found {
    let until = Instant::now() + WALK_BUDGET;
    let mut found = Found {
        items: Vec::new(),
        partial: false,
        cut: false,
    };
    let folders = kind == ContextKind::Project;
    for (walked, entry) in walk(root, None, false).enumerate() {
        if cancelled() || found.items.len() > enough {
            break;
        }
        if walked >= most_walked || Instant::now() > until {
            found.cut = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        let Some(kind) = entry.file_type() else {
            continue;
        };
        let Some(path) = entry
            .path()
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        if path == ".git" || path.starts_with(".git/") || !wanted.within(path) {
            continue;
        }
        let item = if kind.is_file() && !folders {
            ContextCandidate {
                kind: ContextKind::File,
                label: path.into(),
                detail: "Project source".into(),
                source: ContextRef::File {
                    path: path.into(),
                    lines: None,
                },
            }
        } else if kind.is_dir() && folders {
            ContextCandidate {
                kind: ContextKind::Project,
                label: path.into(),
                detail: "Project folder".into(),
                source: ContextRef::Project { path: path.into() },
            }
        } else {
            continue;
        };
        found.items.push(item);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(root: &Path, kind: ContextKind, query: &str) -> SearchJob {
        SearchJob {
            kind,
            query: query.into(),
            offset: 0,
            listed: Listed::default(),
            project: None,
            overlays: crate::outline::Overlays::default(),
            root: root.to_owned(),
            owner: MachineId([7; 16]),
        }
    }

    fn labels(reply: ContextReply) -> (Vec<String>, String) {
        let ContextReply::Candidates { items, notice, .. } = reply else {
            panic!("a search answers with candidates");
        };
        (items.into_iter().map(|item| item.label).collect(), notice)
    }

    #[test]
    fn files_are_found_by_any_case_leaving_out_what_the_project_ignores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("src/Main.rs"), "").unwrap();
        std::fs::write(root.join("out/main.o"), "").unwrap();
        std::fs::write(root.join(".git/main"), "").unwrap();
        std::fs::write(root.join(".gitignore"), "out\n").unwrap();
        let (found, notice) = labels(run(job(root, ContextKind::File, "MAIN"), &|| false).unwrap());
        assert_eq!(found, ["src/Main.rs"]);
        assert_eq!(notice, "");
        let (folders, _) = labels(run(job(root, ContextKind::Project, "s"), &|| false).unwrap());
        assert_eq!(folders, ["src"]);
    }

    #[test]
    fn a_walk_cut_short_says_so_but_one_with_enough_for_the_page_does_not() {
        let dir = tempfile::tempdir().unwrap();
        for number in 0..(3 * PAGE) {
            std::fs::write(dir.path().join(format!("file{number:03}.txt")), "").unwrap();
        }
        let wanted = Wanted::new("file");
        let all = |most_walked| {
            files(
                dir.path(),
                ContextKind::File,
                &wanted,
                usize::MAX,
                most_walked,
                &|| false,
            )
        };
        let whole = all(MOST_WALKED);
        assert_eq!((whole.items.len(), whole.cut), (3 * PAGE, false));
        let short = all(10);
        assert!(short.items.len() < 3 * PAGE);
        assert!(short.cut);
        // A page's worth and more to come is paging, not a cut.
        let reply = run(job(dir.path(), ContextKind::File, "file"), &|| false).unwrap();
        let ContextReply::Candidates {
            items,
            notice,
            more,
        } = reply
        else {
            panic!("a search answers with candidates");
        };
        assert_eq!(items.len(), PAGE);
        assert!(more);
        assert_eq!(notice, "");
    }

    #[test]
    fn results_belong_to_this_machine() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let ContextReply::Candidates { items, .. } =
            run(job(dir.path(), ContextKind::File, ""), &|| false).unwrap()
        else {
            panic!("a search answers with candidates");
        };
        assert!(matches!(
            &items[0].source,
            ContextRef::Owned { machine, .. } if *machine == MachineId([7; 16])
        ));
    }
}
