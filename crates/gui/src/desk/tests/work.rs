use super::*;
use clackatoo_protocol::{WorkAction, WorkState, wire};

#[test]
fn work_badge_counts_active_jobs_and_keeps_failures_visible_until_dismissed() {
    let mut desk = desk(workspaces(vec![workspace(
        1,
        "project",
        vec![tab(1, pane(1))],
    )]));
    assert_eq!(desk.work_badge(), None);
    let mut items = crate::tests::example_work();
    desk.work_changed(items.clone());
    assert_eq!(desk.work_badge(), Some(("Working · 2 !".into(), true)));
    items.retain(|item| !item.state.active());
    desk.work_changed(items);
    assert_eq!(desk.work_badge(), Some(("Work ! 1".into(), true)));
    desk.work_changed(Vec::new());
    assert_eq!(desk.work_badge(), None);
}

#[test]
fn work_picker_updates_live_and_controls_the_selected_job_without_opening_a_file() {
    let mut desk = desk(workspaces(vec![workspace(
        1,
        "project",
        vec![tab(1, pane(1))],
    )]));
    let mut items = crate::tests::example_work();
    desk.work_changed(items.clone());
    normal_mode(&mut desk);
    command(&mut desk, "Work");
    assert_eq!(
        desk.finder.as_ref().unwrap().kind(),
        crate::finder::Kind::Work
    );
    assert!(answer_all(&mut desk).contains(&Request::Work(WorkAction::List)));
    desk.finder.as_mut().unwrap().paste("outline");
    // Enter remains in the work picker; it never treats the ID as a path.
    press(
        &mut desk,
        Key::Named(NamedKey::Enter),
        None,
        Modifiers::default(),
    );
    assert!(desk.finder.is_some());
    ctrl(&mut desk, "d");
    assert!(answer_all(&mut desk).contains(&Request::Work(WorkAction::Cancel(1))));
    items[0].state = WorkState::Completed;
    items[0].cancellable = false;
    desk.work_changed(items);
    ctrl(&mut desk, "d");
    assert!(answer_all(&mut desk).contains(&Request::Work(WorkAction::Dismiss(1))));
}

#[test]
fn automated_input_cannot_cancel_work_and_older_daemons_are_not_asked() {
    let mut desk = desk(workspaces(vec![workspace(
        1,
        "project",
        vec![tab(1, pane(1))],
    )]));
    desk.work_changed(crate::tests::example_work());
    normal_mode(&mut desk);
    command(&mut desk, "Work");
    answer_all(&mut desk);
    desk.automating = true;
    desk.work_action("1");
    assert!(
        !answer_all(&mut desk)
            .iter()
            .any(|request| matches!(request, Request::Work(_)))
    );
    desk.automating = false;
    desk.close_finder();
    desk.daemon_is(wire::WORK - 1, None);
    normal_mode(&mut desk);
    command(&mut desk, "Work");
    assert!(desk.finder.is_none());
    assert!(
        !answer_all(&mut desk)
            .iter()
            .any(|request| matches!(request, Request::Work(_)))
    );
}
