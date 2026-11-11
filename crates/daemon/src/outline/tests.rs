use std::fmt::Write as _;
use std::sync::mpsc;

use super::*;
use crate::work::{Priority, Scheduler};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../syntax/tests/fixtures/languages")
}

fn project(dir: &tempfile::TempDir) -> Arc<Project> {
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    Arc::new(Project::new(root, &dir.path().join("cache"), fixtures()))
}

fn scan(project: &Arc<Project>) -> Status {
    scan_on(project, &mut Scheduler::new(|| {}))
}

fn scan_on(project: &Arc<Project>, scheduler: &mut Scheduler) -> Status {
    let project = Arc::clone(project);
    let (tx, rx) = mpsc::channel();
    let mut scan = None;
    let work = scheduler
        .submit(
            "scan".into(),
            "Indexing outline",
            None,
            Priority::Background,
            move |context| {
                let result = (|| {
                    if scan.is_none() {
                        scan = Some(Scan::new(Arc::clone(&project))?);
                    }
                    scan.as_mut().unwrap().step(context)
                })();
                if matches!(result, Ok(Step::Done) | Err(_)) {
                    tx.send(result.clone().map(|_| project.status())).unwrap();
                }
                result
            },
        )
        .unwrap();
    let result = rx.recv_timeout(Duration::from_mins(3)).unwrap().unwrap();
    let due = Instant::now() + Duration::from_secs(5);
    while scheduler
        .state(work)
        .is_some_and(clackatoo_protocol::WorkState::active)
    {
        assert!(
            Instant::now() < due,
            "index job did not close its resources"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    result
}

fn ask(project: &Arc<Project>, request: Query, overlays: Vec<Overlay>) -> Result<String, String> {
    let project = Arc::clone(project);
    let mut scheduler = Scheduler::new(|| {});
    let (tx, rx) = mpsc::channel();
    scheduler
        .submit(
            "query".into(),
            "Reading outline",
            None,
            Priority::Interactive,
            move |context| {
                tx.send(query(&project, &request, &overlays, context))
                    .unwrap();
                Ok(Step::Done)
            },
        )
        .unwrap();
    rx.recv_timeout(Duration::from_secs(15)).unwrap()
}

fn search(project: &Arc<Project>, query: &str) -> Value {
    serde_json::from_str(
        &ask(
            project,
            Query::Symbols {
                query: query.into(),
                limit: 20,
            },
            Vec::new(),
        )
        .unwrap(),
    )
    .unwrap()
}

use serde_json::Value;

#[test]
fn persists_symbols_reuses_unchanged_files_and_searches_names_and_paths() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    std::fs::create_dir(project.root.join("auth")).unwrap();
    std::fs::write(
        project.root.join("auth/login.curly"),
        "fn authenticate(user) { user; }\nfn logout() {}\n",
    )
    .unwrap();
    let cold = scan(&project);
    assert!(cold.complete, "{cold:?}");
    assert_eq!(cold.parsed, 1);
    let found = search(&project, "auth");
    assert_eq!(found["symbols"].as_array().unwrap().len(), 2);
    assert_eq!(found["symbols"][0]["signature"], "fn authenticate(user)");
    let reopened = Arc::new(Project::new(
        project.root.clone(),
        &dir.path().join("cache"),
        fixtures(),
    ));
    let warm = scan(&reopened);
    assert_eq!((warm.parsed, warm.cache_hits), (0, 1));
    assert_eq!(search(&reopened, "logout")["symbols"][0]["line"], 2);
}

#[test]
fn external_edits_renames_deletions_and_lost_events_reconcile_without_stale_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    let path = project.root.join("a.curly");
    std::fs::write(&path, "fn before() {}\n").unwrap();
    scan(&project);
    std::fs::write(&path, "fn after() {}\n").unwrap();
    project.notifications();
    assert!(project.needs_work(), "inotify should identify the edit");
    let incremental = scan(&project);
    assert_eq!(
        incremental.parsed, 2,
        "one initial parse and one changed file"
    );
    assert!(
        search(&project, "before")["symbols"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        search(&project, "after")["symbols"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::rename(&path, project.root.join("b.curly")).unwrap();
    project.notifications();
    scan(&project);
    assert_eq!(search(&project, "after")["symbols"][0]["path"], "b.curly");
    std::fs::remove_file(project.root.join("b.curly")).unwrap();
    project.changed(None); // The overflow/unwatched reconciliation path.
    scan(&project);
    assert!(
        search(&project, "after")["symbols"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_burst_of_filesystem_events_is_drained_in_bounded_turns_and_requests_reconciliation() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    scan(&project); // Establish the root watch.
    assert!(project.watch.is_some());
    for number in 0..2300 {
        std::fs::write(project.root.join(format!("event-{number}.tmp")), "").unwrap();
    }
    project.notifications();
    assert!(
        project.unread(),
        "the burst should exceed one drain's event budget"
    );
    assert!(project.needs_work());
    for _ in 0..8 {
        if !project.unread() {
            break;
        }
        project.notifications();
    }
    assert!(
        !project.unread(),
        "readiness must eventually be fully drained"
    );
    assert!(
        project.dirty.lock().unwrap().all,
        "reconcile any discarded reader-buffer events"
    );
}

#[test]
fn unsaved_text_overrides_disk_results_without_polluting_the_persistent_index() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    std::fs::write(project.root.join("a.curly"), "fn disk() {}\n").unwrap();
    scan(&project);
    let overlay = Overlay {
        path: "a.curly".into(),
        text: clackatoo_text::Rope::from_str("fn live() {}\n"),
        version: 7,
    };
    let found: Value = serde_json::from_str(
        &ask(
            &project,
            Query::Symbols {
                query: "live".into(),
                limit: 10,
            },
            vec![overlay.clone()],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(found["symbols"][0]["name"], "live");
    assert_eq!(found["symbols"][0]["unsaved"], true);
    let absent: Value = serde_json::from_str(
        &ask(
            &project,
            Query::Symbols {
                query: "disk".into(),
                limit: 10,
            },
            vec![overlay],
        )
        .unwrap(),
    )
    .unwrap();
    assert!(absent["symbols"].as_array().unwrap().is_empty());
    assert_eq!(search(&project, "disk")["symbols"][0]["name"], "disk");
    assert!(
        search(&project, "live")["symbols"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stale_overlay_rows_cannot_crowd_current_results_out_of_a_bounded_query() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    let mut disk = String::new();
    for number in 0..100 {
        writeln!(disk, "fn matching_{number}() {{}}").unwrap();
    }
    std::fs::write(project.root.join("a.curly"), disk).unwrap();
    std::fs::write(project.root.join("b.curly"), "fn matching_current() {}\n").unwrap();
    scan(&project);
    let result: Value = serde_json::from_str(
        &ask(
            &project,
            Query::Symbols {
                query: "matching".into(),
                limit: 1,
            },
            vec![Overlay {
                path: "a.curly".into(),
                text: clackatoo_text::Rope::from_str("fn other() {}\n"),
                version: 1,
            }],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["symbols"][0]["path"], "b.curly");
    assert_eq!(result["symbols"][0]["name"], "matching_current");
}

#[test]
fn ignores_generated_and_excluded_files_and_never_follows_source_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    std::fs::write(project.root.join(".gitignore"), "ignored.curly\n").unwrap();
    std::fs::write(project.root.join("ignored.curly"), "fn hidden() {}\n").unwrap();
    std::fs::create_dir(project.root.join("target")).unwrap();
    std::fs::write(
        project.root.join("target/build.curly"),
        "fn generated() {}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("secret.curly"), "fn secret() {}\n").unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("secret.curly"),
        project.root.join("link.curly"),
    )
    .unwrap();
    scan(&project);
    assert!(
        search(&project, "hidden")["symbols"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        search(&project, "secret")["symbols"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for path in ["link.curly", "../secret.curly", "ignored.curly"] {
        assert!(
            ask(
                &project,
                Query::File {
                    path: path.into(),
                    limit: 20
                },
                Vec::new()
            )
            .is_err(),
            "{path}"
        );
    }
    let overlay = Overlay {
        path: "ignored.curly".into(),
        text: clackatoo_text::Rope::from_str("fn hidden() {}"),
        version: 1,
    };
    assert!(
        ask(
            &project,
            Query::File {
                path: "ignored.curly".into(),
                limit: 20
            },
            vec![overlay]
        )
        .is_err()
    );
}

#[test]
fn targeted_outlines_work_before_the_background_scan_and_maps_have_a_hard_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    let mut text = String::new();
    for number in 0..100 {
        writeln!(text, "fn function_{number}() {{}}").unwrap();
    }
    std::fs::write(project.root.join("a.curly"), text).unwrap();
    let found: Value = serde_json::from_str(
        &ask(
            &project,
            Query::File {
                path: "a.curly".into(),
                limit: 10,
            },
            Vec::new(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(found["symbols"].as_array().unwrap().len(), 10);
    assert_eq!(found["more"], true);
    let map = ask(
        &project,
        Query::Map {
            query: String::new(),
            focus: vec!["a.curly".into()],
            max_bytes: 1024,
        },
        Vec::new(),
    )
    .unwrap();
    assert!(map.len() <= 1024);
    assert!(map.contains("more definitions"));
}

/// Large-scale qualification, kept opt-in because it creates millions of
/// source lines. Prints measurements for cold, warm and one-file passes.
#[test]
#[ignore = "large indexing qualification; run explicitly"]
fn qualifies_three_million_lines() {
    let dir = tempfile::tempdir().unwrap();
    let project = project(&dir);
    let mut source = String::new();
    for symbol in 0..50 {
        writeln!(source, "fn example_{symbol}(value) {{").unwrap();
        for _ in 0..28 {
            source.push_str("value;\n");
        }
        source.push_str("}\n");
    }
    for file in 0..2000 {
        std::fs::write(project.root.join(format!("file-{file:04}.curly")), &source).unwrap();
    }
    let mut worker = Scheduler::new(|| {});
    // Keep the same worker pool alive across passes, as the daemon does.
    let cold_at = Instant::now();
    let cold = scan_on(&project, &mut worker);
    let cold_elapsed = cold_at.elapsed();
    assert!(cold.complete, "{cold:?}");
    assert_eq!(cold.parsed, 2000);
    project.changed(None);
    let warm_at = Instant::now();
    let warm = scan_on(&project, &mut worker);
    let warm_elapsed = warm_at.elapsed();
    assert_eq!(warm.parsed, 0);
    assert_eq!(warm.cache_hits, 2000);
    std::fs::write(project.root.join("file-0000.curly"), "fn updated() {}\n").unwrap();
    project.notifications();
    let edit_at = Instant::now();
    let edited = scan_on(&project, &mut worker);
    let edit_elapsed = edit_at.elapsed();
    assert_eq!(edited.parsed, 1);
    let query_at = Instant::now();
    assert_eq!(search(&project, "updated")["symbols"][0]["name"], "updated");
    let query_elapsed = query_at.elapsed();
    let map_at = Instant::now();
    let map = ask(
        &project,
        Query::Map {
            query: String::new(),
            focus: Vec::new(),
            max_bytes: 12000,
        },
        Vec::new(),
    )
    .unwrap();
    assert!(map.contains("example") && map.len() <= 12000);
    eprintln!(
        "3,000,000 lines; cold={cold_elapsed:?}; warm={warm_elapsed:?}; one-file={edit_elapsed:?}; search={query_elapsed:?}; map={:?}; db={} bytes",
        map_at.elapsed(),
        project.database.metadata().unwrap().len()
    );
}
