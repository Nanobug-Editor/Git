//! Pure lifecycle regressions exercise cleanup authority without calling native WIT imports.
use super::*;

/// Capability IDs stay visibly distinct from native document identities in these state tests.
fn handle(resource: u64) -> api::ResourceHandle {
    api::ResourceHandle {
        instance: "diff-lifecycle".into(),
        scope: "workspace".into(),
        resource,
    }
}

/// An owned immutable source exposes exactly the identity and resource returned by the real editor.
fn source(id: &str, revision: u64, resource: u64) -> api::DocumentInfo {
    api::DocumentInfo {
        document: api::DocumentVersion {
            id: id.into(),
            path: format!("virtual:{id}"),
            revision,
        },
        resource: api::ResourceIdentity::Virtual {
            handle: handle(resource),
        },
        title: id.into(),
        access: api::DocumentAccess {
            read: true,
            edit: false,
            save: false,
        },
        dirty: false,
        language: "text".into(),
        encoding: api::DocumentEncoding::Utf8,
        eol: api::DocumentEol::Lf,
        byte_len: 0,
        selection: api::TextRange { start: 0, end: 0 },
        visible_rows: None,
    }
}

/// Register a peer using the same correlation and capability bookkeeping as State::editor.
fn pending(state: &mut State, resource: u64, intent: &str, new_source: bool) {
    state.editor_tasks.insert(resource, intent.into());
    state.track_diff_request(handle(resource), new_source);
}

/// Every comparison stage can time out; no retired peer may subsequently summon a tab.
#[test]
fn cancelled_diff_retires_peer_requests_new_sources_and_late_receipts() {
    for intent in [
        "diff-open",
        "diff-left",
        "diff-right",
        "diff-third",
        "compare",
    ] {
        let mut state = State {
            diff_key: "selected-commit-file".into(),
            diff_history: true,
            diff_mode: Some(true),
            ..Default::default()
        };
        state.begin_diff_flight(true);
        let created = source("new-left", 1, 200);
        state.observe_diff_source(&created, true);
        state.diff_views.insert(
            state.diff_key.clone(),
            DiffSources {
                left: Some(created.document.clone()),
                ..Default::default()
            },
        );
        pending(&mut state, 10, intent, false);
        pending(&mut state, 11, "diff-right", true);
        state.editor_tasks.insert(99, "visibility".into());
        state.left_version = Some(created.document.clone());
        state.right_version = Some(source("result", 1, 201).document);
        state.third_version = Some(source("third", 1, 202).document);
        let error = api::Failure::new(api::ErrorCode::TimedOut, "comparison timed out");
        let cleanup = state.take_diff_cleanup(Some(&error));
        assert_eq!(
            cleanup
                .iter()
                .map(|handle| handle.resource)
                .collect::<Vec<_>>(),
            [10, 11, 200]
        );
        assert_eq!(
            state.editor_tasks,
            BTreeMap::from([(99, "visibility".to_owned())])
        );
        assert!(
            state.left_version.is_none()
                && state.right_version.is_none()
                && state.third_version.is_none()
        );
        assert!(state.diff_mode.is_none() && !state.diff_history && !state.busy());
        assert!(state.diff_views.is_empty());
        assert!(state.history_error.contains("comparison timed out"));
        // The actual event branch returns before WIT access once its correlation is retired.
        state
            .event(
                None,
                api::Notification::Request {
                    handle: handle(10),
                    update: api::RequestUpdate::Completed {
                        result: Ok(api::EditorValue::DocumentOpened(created)),
                    },
                },
            )
            .unwrap();
        assert!(state.editor_tasks.get(&10).is_none());
        assert!(
            state.left_version.is_none()
                && state.right_version.is_none()
                && state.third_version.is_none()
        );
        assert!(state.diff_views.is_empty());
    }
}

/// A failed new pane must not revoke an already open comparison or restore a stale cached revision.
#[test]
fn failed_diff_preserves_old_tabs_and_refreshed_source_revisions() {
    let old_left = source("old-left", 1, 1);
    let old_result = source("old-result", 1, 2);
    let old_third = source("old-third", 1, 3);
    let mut state = State {
        diff_key: "retained".into(),
        ..Default::default()
    };
    state.diff_views.insert(
        "retained".into(),
        DiffSources {
            left: Some(old_left.document.clone()),
            right: Some(old_result.document.clone()),
            third: Some(old_third.document.clone()),
            navigation: None,
        },
    );
    state.diff_views.insert(
        "other-commit".into(),
        DiffSources {
            left: Some(old_left.document.clone()),
            ..Default::default()
        },
    );
    state.begin_diff_flight(true);
    let refreshed_left = source("old-left", 2, 1);
    state.observe_diff_source(&refreshed_left, false);
    let provisional = source("provisional-result", 1, 200);
    state.observe_diff_source(&provisional, true);
    state.diff_views.get_mut("retained").unwrap().right = Some(provisional.document);
    let error = api::Failure::new(
        api::ErrorCode::LimitExceeded,
        "comparison tab quota exceeded",
    );
    let cleanup = state.take_diff_cleanup(Some(&error));
    assert_eq!(cleanup, [handle(200)]);
    let retained = &state.diff_views["retained"];
    assert_eq!(retained.left.as_ref(), Some(&refreshed_left.document));
    assert_eq!(retained.right.as_ref(), Some(&old_result.document));
    assert_eq!(retained.third.as_ref(), Some(&old_third.document));
    assert!(state.diff_views.contains_key("other-commit"));
    assert!(
        state
            .history_error
            .contains("comparison tab quota exceeded")
    );
}

/// Final native admission transfers new sources to the descriptor instead of leaving hidden orphans.
#[test]
fn successful_compare_transfers_three_new_sources_only_at_its_final_receipt() {
    let mut state = State {
        diff_key: "three-source".into(),
        diff_history: true,
        ..Default::default()
    };
    state.begin_diff_flight(true);
    for (id, resource) in [("left", 200), ("result", 201), ("third", 202)] {
        state.observe_diff_source(&source(id, 1, resource), true);
    }
    pending(&mut state, 10, "compare", false);
    state.diff_mode = None;
    assert!(
        state.diff_history,
        "sending the final operation must retain historical error presentation"
    );
    state.editor_tasks.remove(&10);
    state.take_diff_request(10);
    state.complete_diff_flight();
    assert!(!state.diff_history && state.diff_flight.is_none());
    assert!(
        state
            .take_diff_cleanup(Some(&failure("later unrelated failure")))
            .is_empty()
    );
    assert!(state.history_error.is_empty());
}

/// Source-close ingress revokes the old cache independently of an in-flight replacement's failure.
#[test]
fn failed_replacement_never_restores_a_closed_original_source() {
    let old = source("closed-source", 1, 1);
    let mut state = State {
        diff_key: "retained".into(),
        ..Default::default()
    };
    state.diff_views.insert(
        "retained".into(),
        DiffSources {
            left: Some(old.document.clone()),
            ..Default::default()
        },
    );
    state.begin_diff_flight(true);
    state.forget_closed_diff_source(&old.document);
    state.observe_diff_source(&source("new-source", 1, 200), true);
    assert_eq!(
        state.take_diff_cleanup(Some(&failure("replacement failed"))),
        [handle(200)]
    );
    assert!(!state.diff_views.contains_key("retained"));
}
