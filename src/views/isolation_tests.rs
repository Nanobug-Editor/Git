//! Ordinary panel interaction must not replace another panel's admitted scene or cancel its reads.
use super::*;

/// Exercise the same semantic publication boundary as the exported guest dispatcher.
fn publish(state: &mut State) -> Vec<api::View> {
    let views = state.views();
    state.changed_views(views)
}

/// A draft change owns only the composer; repeating an unchanged publication owns no scene.
#[test]
fn panel_isolation_draft_keeps_history_scene_admitted() {
    let mut state = State::default();
    assert_eq!(publish(&mut state).len(), 4);
    state
        .ui_event(
            Some("staging"),
            ui::UiEvent {
                revision: state.revision,
                node: "commit-message".into(),
                action: ui::Action::Change("保留这份草稿".into()),
            },
        )
        .unwrap();
    let changed = publish(&mut state);
    assert_eq!(
        changed
            .iter()
            .map(|view| view.panel.as_str())
            .collect::<Vec<_>>(),
        ["staging"]
    );
    assert!(
        publish(&mut state).is_empty(),
        "unchanged documents must retain their revision"
    );
}

/// An obsolete feedback click has no action and cannot cancel an unrelated immutable commit read.
#[test]
fn panel_isolation_commit_details_keep_history_query() {
    let mut state = State::default();
    state.queue.push_back(Command::new(
        ["show", "--format=%B", "HEAD"],
        Purpose::CommitMessage("oid".into()),
    ));
    state.message = "operation details".into();
    let event = ui::UiEvent {
        revision: 0,
        node: "status".into(),
        action: ui::Action::Click,
    };
    assert!(
        ui::Document::new(state.commit_panel())
            .validate_event(&event)
            .is_err()
    );
    assert!(!state.commit_panel_event(&event).unwrap());
    assert!(state.commit_hunk_dialog().is_none());
    assert_eq!(
        state.queue.len(),
        1,
        "opening a staging dialog cancelled the history query"
    );
}

/// A staging write may disable its own controls while history browsing keeps the same scene.
#[test]
fn panel_isolation_index_write_preserves_history_and_avoids_log_reload() {
    let mut state = State::default();
    state.commit_message = "draft".into();
    publish(&mut state);
    state
        .queue
        .push_back(Command::new(["add", "--", "file.txt"], Purpose::Action));
    let changed = publish(&mut state);
    assert!(!changed.iter().any(|view| view.panel == "history"));
    state.queue.clear();
    state.refresh_worktree();
    assert!(
        state.queue.iter().all(|command| matches!(
            command.purpose,
            Purpose::Status { .. } | Purpose::OperationProbe { .. }
        )),
        "index writes must not enqueue history or ref queries"
    );
    assert_eq!(state.commit_message, "draft");
}

/// Opposite-surface file clicks queue their comparison without dropping the current immutable read.
#[test]
fn panel_isolation_file_clicks_preserve_other_surface_work() {
    let mut state = State::default();
    state.queue.push_back(Command::new(
        ["show", "HEAD"],
        Purpose::CommitMessage("oid".into()),
    ));
    state
        .request_comparison(panel_activity::ComparisonSelection::Worktree {
            path: "first".into(),
            staged: false,
        })
        .unwrap();
    state
        .request_comparison(panel_activity::ComparisonSelection::History {
            oid: "oid".into(),
            path: "old".into(),
        })
        .unwrap();
    state
        .request_comparison(panel_activity::ComparisonSelection::Worktree {
            path: "latest".into(),
            staged: false,
        })
        .unwrap();
    assert_eq!(state.queue.len(), 1);
    assert_eq!(state.pending_comparisons.len(), 2);
    assert!(
        matches!(state.pending_comparisons.back(), Some(panel_activity::ComparisonSelection::Worktree { path, .. }) if path == "latest")
    );
}

/// A failed history action reports locally without clearing a staged-operation message or draft.
#[test]
fn panel_isolation_history_feedback_keeps_composer() {
    let mut state = State::default();
    state.message = "staged files ready".into();
    state.commit_message = "draft".into();
    publish(&mut state);
    let previous = std::mem::replace(&mut state.message, "history lookup failed".into());
    state.retain_panel_feedback(panel_activity::FeedbackOwner::History, &previous);
    let changed = publish(&mut state);
    assert!(!changed.iter().any(|view| view.panel == "staging"));
    assert!(
        ui::Document::new(state.commit_panel())
            .active_node("status")
            .is_none()
    );
    assert_eq!(state.commit_message, "draft");
}

/// Dismissing history cannot consume the composer's independently open hunk controls.
#[test]
fn panel_isolation_dialog_dismissal_stays_in_its_panel() {
    let mut state = State::default();
    state.commit_hunks_open = true;
    state
        .ui_event(
            Some("history"),
            ui::UiEvent {
                revision: 0,
                node: "branch-dialog".into(),
                action: ui::Action::Dismiss,
            },
        )
        .unwrap();
    assert!(state.commit_hunks_open);
}
