//! Execution races and scheduler boundaries, using channels instead of timing.

use std::sync::mpsc::{self, Receiver, Sender};

use super::*;

fn until<T>(rx: &Receiver<T>) -> T {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("worker replied")
}

fn blocking(scheduler: &mut Scheduler, name: &str, scope: &str) -> (u64, Sender<()>) {
    let (release, wait) = mpsc::channel();
    let (started, start) = mpsc::channel();
    let id = scheduler
        .submit(
            name.into(),
            name,
            Some(scope.into()),
            Priority::Background,
            move |_| {
                started.send(()).unwrap();
                until(&wait);
                Ok(Step::Done)
            },
        )
        .unwrap();
    until(&start);
    (id, release)
}

fn settle(scheduler: &Scheduler, id: u64) -> WorkState {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let state = scheduler.state(id).expect("job exists");
        if !state.active() {
            return state;
        }
        assert!(Instant::now() < until, "job did not settle: {state:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn duplicate_consumers_share_execution_and_promote_the_queued_job() {
    let mut scheduler = Scheduler::new(|| {});
    let (_, release_a) = blocking(&mut scheduler, "busy-a", "a");
    let (_, release_b) = blocking(&mut scheduler, "busy-b", "b");
    let (ran, rx) = mpsc::channel();
    let first = scheduler
        .submit(
            "shared".into(),
            "Scan",
            Some("c".into()),
            Priority::Background,
            move |_| {
                ran.send(()).unwrap();
                Ok(Step::Done)
            },
        )
        .unwrap();
    let second = scheduler
        .submit(
            "shared".into(),
            "Scan",
            Some("c".into()),
            Priority::Interactive,
            |_| panic!("a duplicate must never execute"),
        )
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        scheduler.shared.lock().entries[&first].priority,
        Priority::Interactive
    );
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    until(&rx);
    assert_eq!(settle(&scheduler, first), WorkState::Completed);
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_project_runs_one_job_at_a_time_and_queued_cancellation_never_executes() {
    let mut scheduler = Scheduler::new(|| {});
    let (_, release) = blocking(&mut scheduler, "busy", "same");
    let queued = scheduler
        .submit(
            "queued".into(),
            "Queued",
            Some("same".into()),
            Priority::Interactive,
            |_| panic!("cancelled queued job ran"),
        )
        .unwrap();
    // The second worker can run another project while this project's work waits.
    let other = scheduler
        .submit(
            "other".into(),
            "Other",
            Some("other".into()),
            Priority::Background,
            |_| Ok(Step::Done),
        )
        .unwrap();
    assert_eq!(settle(&scheduler, other), WorkState::Completed);
    assert_eq!(scheduler.state(queued), Some(WorkState::Queued));
    scheduler.cancel(queued).unwrap();
    assert_eq!(scheduler.state(queued), Some(WorkState::Cancelling));
    release.send(()).unwrap();
    assert_eq!(settle(&scheduler, queued), WorkState::Cancelled);
}

#[test]
fn cancellation_wins_over_late_success_and_a_panicking_job_does_not_kill_the_pool() {
    let mut scheduler = Scheduler::new(|| {});
    let (id, release) = blocking(&mut scheduler, "busy", "project");
    scheduler.cancel(id).unwrap();
    assert_eq!(scheduler.state(id), Some(WorkState::Cancelling));
    assert!(scheduler.dismiss(id).is_err());
    release.send(()).unwrap();
    assert_eq!(settle(&scheduler, id), WorkState::Cancelled);
    let failed = scheduler
        .submit("panic".into(), "Panics", None, Priority::Background, |_| {
            panic!("a bad feature")
        })
        .unwrap();
    assert_eq!(settle(&scheduler, failed), WorkState::Failed);
    let good = scheduler
        .submit("good".into(), "Good", None, Priority::Background, |_| {
            Ok(Step::Done)
        })
        .unwrap();
    assert_eq!(settle(&scheduler, good), WorkState::Completed);
    scheduler.dismiss(failed).unwrap();
    assert_eq!(scheduler.state(failed), None);
}

#[test]
fn work_can_yield_and_report_an_unknown_then_known_total() {
    let mut scheduler = Scheduler::new(|| {});
    let mut phase = 0;
    let id = scheduler
        .submit(
            "phases".into(),
            "Phases",
            None,
            Priority::Background,
            move |context| {
                assert!(!context.cancelled());
                phase += 1;
                if phase == 1 {
                    context.progress("Discovering", 4, None);
                    Ok(Step::Again)
                } else {
                    context.progress("Parsing", 20, Some(10));
                    Ok(Step::Done)
                }
            },
        )
        .unwrap();
    assert_eq!(settle(&scheduler, id), WorkState::Completed);
    let item = scheduler
        .snapshot()
        .1
        .into_iter()
        .find(|item| item.id == id)
        .unwrap();
    assert_eq!(
        (item.detail.as_str(), item.completed, item.total),
        ("Parsing", 10, Some(10))
    );
}

#[test]
fn queue_and_history_are_bounded_and_successes_do_not_hide_failures() {
    let mut scheduler = Scheduler::new(|| {});
    let (_, release_a) = blocking(&mut scheduler, "busy-a", "a");
    let (_, release_b) = blocking(&mut scheduler, "busy-b", "b");
    let mut queued = Vec::new();
    for number in 0..MOST_ACTIVE - WORKERS {
        queued.push(
            scheduler
                .submit(
                    format!("job-{number}"),
                    "Job",
                    None,
                    Priority::Background,
                    |_| Ok(Step::Done),
                )
                .unwrap(),
        );
    }
    assert!(
        scheduler
            .submit(
                "overflow".into(),
                "Overflow",
                None,
                Priority::Background,
                |_| Ok(Step::Done)
            )
            .is_err()
    );
    for &id in &queued {
        scheduler.cancel(id).unwrap();
    }
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while scheduler
        .snapshot()
        .1
        .iter()
        .any(|item| item.state.active())
    {
        assert!(Instant::now() < until, "cancelled jobs did not close");
        std::thread::sleep(Duration::from_millis(1));
    }
    let failed = scheduler
        .submit(
            "failed".into(),
            "Failed",
            None,
            Priority::Background,
            |_| Err("real failure".into()),
        )
        .unwrap();
    assert_eq!(settle(&scheduler, failed), WorkState::Failed);
    for number in 0..MOST_HISTORY + 2 {
        let id = scheduler
            .submit(
                format!("success-{number}"),
                "Success",
                None,
                Priority::Background,
                |_| Ok(Step::Done),
            )
            .unwrap();
        settle(&scheduler, id);
    }
    assert_eq!(scheduler.state(failed), Some(WorkState::Failed));
    assert!(scheduler.snapshot().1.len() <= MOST_HISTORY);
}

#[test]
fn aging_and_scope_rotation_prevent_queue_starvation() {
    let mut state = State::default();
    for (id, scope, priority) in [
        (1, "a", Priority::Interactive),
        (2, "b", Priority::Interactive),
        (3, "c", Priority::Background),
    ] {
        state.entries.insert(
            id,
            Entry {
                key: id.to_string(),
                priority,
                ready: Instant::now().checked_sub(AGE).unwrap(),
                item: WorkItem {
                    id,
                    title: "Job".into(),
                    scope: Some(scope.into()),
                    state: WorkState::Queued,
                    detail: String::new(),
                    completed: 0,
                    total: None,
                    cancellable: true,
                },
                task: None,
            },
        );
        state.queue.push_back(id);
    }
    state.last_scope = Some("a".into());
    assert_eq!(pick(&state), Some(1));
    state.queue.remove(1);
    state.last_scope = Some("a".into());
    assert_eq!(pick(&state), Some(1), "aged background scope gets a turn");
}

#[test]
fn scope_is_not_released_until_the_jobs_resources_have_closed() {
    struct Cleanup {
        began: Sender<()>,
        wait: Receiver<()>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.began.send(()).unwrap();
            until(&self.wait);
        }
    }
    let mut scheduler = Scheduler::new(|| {});
    let (began, closing) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let cleanup = Cleanup { began, wait };
    scheduler
        .submit(
            "owner".into(),
            "Owns resources",
            Some("project".into()),
            Priority::Background,
            move |_| {
                let _keep = &cleanup;
                Ok(Step::Done)
            },
        )
        .unwrap();
    until(&closing);
    let (ran, next) = mpsc::channel();
    scheduler
        .submit(
            "reader".into(),
            "Uses resources",
            Some("project".into()),
            Priority::Interactive,
            move |_| {
                ran.send(()).unwrap();
                Ok(Step::Done)
            },
        )
        .unwrap();
    assert!(next.recv_timeout(Duration::from_millis(20)).is_err());
    release.send(()).unwrap();
    until(&next);
}

#[test]
fn queued_cancellation_closes_resources_on_a_worker_without_running_the_task() {
    struct Resource(mpsc::Sender<std::thread::ThreadId>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.send(std::thread::current().id()).unwrap();
        }
    }
    let mut scheduler = Scheduler::new(|| {});
    let (_, release) = blocking(&mut scheduler, "busy", "same");
    let (closed, rx) = mpsc::channel();
    let resource = Resource(closed);
    let id = scheduler
        .submit(
            "queued-cleanup".into(),
            "Queued",
            Some("same".into()),
            Priority::Background,
            move |_| {
                let _ = &resource;
                panic!("cancelled task executed");
            },
        )
        .unwrap();
    scheduler.cancel(id).unwrap();
    assert!(rx.try_recv().is_err());
    release.send(()).unwrap();
    assert_ne!(until(&rx), std::thread::current().id());
    assert_eq!(settle(&scheduler, id), WorkState::Cancelled);
}
