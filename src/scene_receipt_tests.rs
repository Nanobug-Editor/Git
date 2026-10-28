//! Skipped timer observations keep the currently painted native scene's input authority.
use super::*;

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
            &GitPlugin::dispatch(serde_json::to_string(&invocation).unwrap()).unwrap(),
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
            &GitPlugin::dispatch(serde_json::to_string(&input).unwrap()).unwrap(),
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
