//! Skipped timer observations keep the currently painted native scene's input authority.
use super::*;

/// A native comparison's large source must not also be serialized into its hidden legacy window.
#[test]
fn hidden_comparison_windows_do_not_serialize_source_snapshots() {
    let text = "hidden-source-中文\\\"\n".repeat(10_000);
    let mut state = State {
        columns: vec![text.clone(), text],
        ..Default::default()
    };
    let views = state.views();
    let hidden = views
        .iter()
        .find(|view| view.panel == "commit-details")
        .unwrap();
    let encoded = serde_json::to_string(hidden).unwrap();
    assert!(
        !encoded.contains("hidden-source"),
        "hidden snapshots consumed the guest serialization budget"
    );
    assert!(encoded.len() < 4096);
    // Explicitly opening the secondary view restores its complete original sources.
    state.visible_panels.insert("commit-details".into(), true);
    let visible = state
        .views()
        .into_iter()
        .find(|view| view.panel == "commit-details")
        .unwrap();
    assert!(visible.document.active_node("snapshot-0").is_some());
    assert!(
        serde_json::to_string(&visible)
            .unwrap()
            .contains("hidden-source")
    );
}

/// Native focus/viewport observations must not rebuild Git's retained business scenes.
#[test]
fn passive_document_dispatch_keeps_the_admitted_scene() {
    let subscription = api::ResourceHandle {
        instance: "passive-documents".into(),
        scope: "workspace".into(),
        resource: 2,
    };
    STATE.with(|cell| {
        *cell.borrow_mut() = State {
            revision: 7,
            document_subscription: Some(subscription.clone()),
            ..Default::default()
        };
    });
    for sequence in 1..=32 {
        let document = api::DocumentVersion {
            id: "unrelated-native-source".into(),
            path: "unrelated.txt".into(),
            revision: 1,
        };
        let kind = match sequence % 3 {
            0 => api::DocumentEventKind::ActiveChanged(Some(document)),
            1 => api::DocumentEventKind::ViewportChanged {
                document,
                visible_rows: None,
            },
            _ => api::DocumentEventKind::SelectionChanged {
                document,
                selection: api::TextRange {
                    start: 0,
                    end: sequence as usize,
                },
            },
        };
        let invocation = api::Invocation {
            id: sequence,
            message: api::Input::Event {
                panel: None,
                event: api::Notification::DocumentEvent {
                    subscription: subscription.clone(),
                    event: api::DocumentEvent {
                        sequence,
                        resource: None,
                        kind,
                    },
                },
            },
        };
        let reply: api::Completion = serde_json::from_str(
            &dispatch_with(
                serde_json::to_string(&invocation).unwrap(),
                |_, _| panic!("passive metadata must not start Git work"),
                feedback::accept,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(reply.result.unwrap().views.is_empty());
        STATE.with(|cell| assert_eq!(cell.borrow().revision, 7));
    }
    STATE.with(|cell| *cell.borrow_mut() = State::default());
}

/// Stream loss belongs to host diagnostics, not a permanent error beneath commit metadata.
#[test]
fn document_stream_failure_is_absent_from_history_business_view() {
    let subscription = api::ResourceHandle {
        instance: "failed-documents".into(),
        scope: "workspace".into(),
        resource: 2,
    };
    let mut state = State {
        document_subscription: Some(subscription.clone()),
        ..Default::default()
    };
    state
        .event(
            None,
            api::Notification::SubscriptionFailed {
                subscription,
                error: api::Failure::new(
                    api::ErrorCode::LimitExceeded,
                    "Document stream overflow; enumerate and resubscribe",
                ),
            },
        )
        .unwrap();
    state.retain_panel_feedback(panel_activity::FeedbackOwner::Shared, "");
    let document = ui::Document::new(state.history_preview());
    assert!(document.active_node("history-status").is_none());
    assert!(document.active_node("history-error").is_none());
    let _ = feedback::take_notices();
    state.collect_feedback("", None);
    state.flush_feedback(feedback::accept).unwrap();
    assert!(feedback::take_notices().iter().any(|notice| matches!(notice,
        plugin_protocol::interaction::Operation::PublishMessage { severity: Severity::Warning, message, .. }
            if message.contains("Document stream overflow")
    )));
}

/// Foreign resources are filtered before the dispatcher can preempt the current readonly query.
#[test]
fn unrelated_resource_dispatch_preserves_inflight_background_query_and_scene() {
    let handle = api::ResourceHandle {
        instance: "foreign-instance".into(),
        scope: "workspace".into(),
        resource: 3,
    };
    let events = [
        api::Notification::Timer {
            handle: handle.clone(),
            sequence: 1,
        },
        api::Notification::SubscriptionFailed {
            subscription: handle.clone(),
            error: api::Failure::new(api::ErrorCode::LimitExceeded, "foreign overflow"),
        },
        api::Notification::DocumentEvent {
            subscription: handle.clone(),
            event: api::DocumentEvent {
                sequence: 1,
                resource: None,
                kind: api::DocumentEventKind::Closed(api::DocumentVersion {
                    id: "foreign-source".into(),
                    path: "tracked.txt".into(),
                    revision: 1,
                }),
            },
        },
    ];
    for event in events {
        STATE.with(|cell| {
            *cell.borrow_mut() = State {
                revision: 7,
                background_read: true,
                automatic_observation: true,
                queue: VecDeque::from([Command::new(["status"], Purpose::Refs)]),
                running: Some(Running {
                    handle: handle.clone(),
                    command: Command::new(["log"], Purpose::Refs),
                    output: Vec::new(),
                    stderr: Vec::new(),
                }),
                ..Default::default()
            }
        });
        let invocation = api::Invocation {
            id: 1,
            message: api::Input::Event { panel: None, event },
        };
        let reply: api::Completion = serde_json::from_str(
            &feedback::dispatch(serde_json::to_string(&invocation).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(reply.result.unwrap().views.is_empty());
        let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        assert_eq!(state.revision, 7);
        assert_eq!(state.running.unwrap().handle, handle);
        assert_eq!(
            state.queue.len(),
            1,
            "foreign receipts cannot cancel or pump another query"
        );
        assert!(state.background_read && state.automatic_observation);
        assert!(state.refresh_quiet_at.is_none());
    }
}

/// An unrelated resource must not enqueue even the first Git query or replace the scene.
#[test]
fn unrelated_timer_does_not_refresh_visible_workbench() {
    let mut state = State {
        visible_panels: BTreeMap::from([("history".into(), true)]),
        ..Default::default()
    };
    let publish = state
        .event(
            None,
            api::Notification::Timer {
                handle: api::ResourceHandle {
                    instance: "other-feature".into(),
                    scope: "workspace".into(),
                    resource: 1,
                },
                sequence: 1,
            },
        )
        .unwrap();
    assert!(
        !publish,
        "an unrelated timer cannot replace the admitted scene"
    );
    assert!(
        state.queue.is_empty(),
        "an unrelated timer cannot start repository discovery"
    );
}

/// Both hidden workbenches and a completed refresh's cooldown skip real Git work. The public
/// serialized entry point must also retain their current scene, even when the timer fires.
#[test]
fn skipped_timer_dispatch_preserves_the_admitted_scene() {
    for visible in [true, false] {
        STATE.with(|cell| {
            *cell.borrow_mut() = State {
                revision: 7,
                refresh_timer: Some(api::ResourceHandle {
                    instance: "skipped-timer-test".into(),
                    scope: "workspace".into(),
                    resource: 1,
                }),
                visible_panels: BTreeMap::from([("history".into(), visible)]),
                history_refreshed_at: visible.then(std::time::Instant::now),
                ..Default::default()
            };
        });
        let input = api::Invocation {
            id: 1,
            message: api::Input::Event {
                panel: None,
                event: api::Notification::Timer {
                    handle: api::ResourceHandle {
                        instance: "skipped-timer-test".into(),
                        scope: "workspace".into(),
                        resource: 1,
                    },
                    sequence: 1,
                },
            },
        };
        let completion: api::Completion = serde_json::from_str(
            &feedback::dispatch(serde_json::to_string(&input).unwrap()).unwrap(),
        )
        .unwrap();
        let output = completion.result.unwrap();
        let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        assert!(
            output.views.is_empty(),
            "visible={visible}: a skipped timer cannot replace the scene"
        );
        assert_eq!(state.revision, 7);
        assert!(state.queue.is_empty());
        assert!(state.running.is_none());
    }
}

/// Hidden panels keep observing source closure; foreign subscriptions cannot evict a comparison.
#[test]
fn document_close_only_releases_owned_comparison_from_matching_subscription() {
    let subscription = api::ResourceHandle {
        instance: "git-documents".into(),
        scope: "workspace".into(),
        resource: 2,
    };
    let document = api::DocumentVersion {
        id: "comparison-source".into(),
        path: "tracked.txt".into(),
        revision: 1,
    };
    let mut state = State {
        document_subscription: Some(subscription.clone()),
        diff_views: BTreeMap::from([(
            "comparison".into(),
            DiffSources {
                left: Some(document.clone()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let closed = |subscription| api::Notification::DocumentEvent {
        subscription,
        event: api::DocumentEvent {
            sequence: 1,
            resource: None,
            kind: api::DocumentEventKind::Closed(document.clone()),
        },
    };
    let mut foreign = subscription.clone();
    foreign.instance = "another-instance".into();
    assert!(!state.event(None, closed(foreign)).unwrap());
    assert!(state.diff_views.contains_key("comparison"));
    state.event(None, closed(subscription)).unwrap();
    assert!(
        state.diff_views.is_empty(),
        "hidden panels must release closed comparison sources"
    );
    assert!(state.queue.is_empty());
}

/// A terminal subscription receipt withdraws its matching identity without automatic resubscription.
#[test]
fn subscription_failure_ignores_foreign_owner_and_stops_matching_stale_documents() {
    let subscription = api::ResourceHandle {
        instance: "git-documents".into(),
        scope: "workspace".into(),
        resource: 2,
    };
    let mut state = State {
        document_subscription: Some(subscription.clone()),
        ..Default::default()
    };
    let failed = |subscription| api::Notification::SubscriptionFailed {
        subscription,
        error: api::Failure::new(
            api::ErrorCode::LimitExceeded,
            "Document event queue overflow",
        ),
    };
    let mut foreign = subscription.clone();
    foreign.scope = "another-workspace".into();
    assert!(!state.event(None, failed(foreign)).unwrap());
    assert_eq!(state.document_subscription, Some(subscription.clone()));
    state.event(None, failed(subscription.clone())).unwrap();
    assert!(state.document_subscription.is_none());
    assert!(!state.event(None, failed(subscription)).unwrap());
    assert!(
        state.queue.is_empty(),
        "terminal failure cannot schedule resubscription or Git work"
    );
    assert!(state.message.contains("Document event queue overflow"));
}
