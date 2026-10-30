//! Cold reconstruction and hostile persisted data exercise the activation decoder.
use super::*;

/// A fresh instance restores nested axes and columns without a hot-update snapshot.
#[test]
fn cold_restart_restores_nested_splits_and_column_intent() {
    let saved = Saved {
        schema: 1,
        splits: BTreeMap::from([
            ("history-body".into(), vec![0.2, 0.5, 0.3]),
            ("history-preview".into(), vec![0.6, 0.4]),
        ]),
        columns: Some([220., 90., 160.]),
    };
    let bytes = serde_json::to_vec(&saved).unwrap();
    let mut fresh = Preferences::default();
    fresh
        .read(api::PreferenceValue {
            revision: 7,
            data: Some(serde_json::from_slice(&bytes).unwrap()),
        })
        .unwrap();
    let mut state = State::default();
    state.layout_preferences = fresh;
    state.history_columns.restore(Some(&serde_json::json!(
        state.layout_preferences.saved.columns
    )));
    let mut document = ui::Document::new(state.views().remove(1).document.root);
    state.layout_preferences.decorate(&mut document);
    assert_eq!(
        document
            .active_node("history-body")
            .unwrap()
            .layout
            .split_proportions,
        Some(vec![0.2, 0.5, 0.3])
    );
    assert_eq!(
        document
            .active_node("history-preview")
            .unwrap()
            .layout
            .split_proportions,
        Some(vec![0.6, 0.4])
    );
    assert_eq!(state.history_columns.trailing, Some([220., 90., 160.]));
    document.validate().unwrap();
    assert_eq!(state.layout_preferences.revision, 7);
    assert!(state.queue.is_empty());
}

/// Schema/weight failures cannot partially overwrite already admitted intent.
#[test]
fn invalid_persisted_layout_is_rejected_without_partial_restore() {
    for data in [
        serde_json::json!({"schema":2}),
        serde_json::json!({"schema":1,"splits":{"history-body":[0.1,0.2]}}),
        serde_json::json!({"schema":1,"splits":{"history-body":[-0.2,1.2]}}),
        serde_json::json!({"schema":1,"columns":[1e20,20,30]}),
    ] {
        let mut preferences = Preferences::default();
        assert!(
            preferences
                .read(api::PreferenceValue {
                    revision: 1,
                    data: Some(data)
                })
                .is_err()
        );
        assert!(!preferences.loaded);
        assert_eq!(preferences.revision, 0);
    }
}

/// A layout gesture must not preempt a pending Git query or change current selection.
#[test]
fn split_resize_preserves_pending_history_and_selection() {
    let mut state = State::default();
    let views = state.views();
    state.changed_views(views);
    state.selected = "unchanged".into();
    state
        .queue
        .push_back(Command::new(["show-ref"], Purpose::Refs));
    let event = ui::UiEvent {
        revision: state.revision,
        node: "history-body".into(),
        action: ui::Action::SplitResized(vec![0.2, 0.5, 0.3]),
    };
    let input = api::Input::Event {
        panel: Some("history".into()),
        event: api::Notification::Ui(event.clone()),
    };
    assert!(!observation::interrupts(&input));
    state.ui_event(Some("history"), event).unwrap();
    assert_eq!(state.queue.len(), 1);
    assert_eq!(state.selected, "unchanged");
    assert_eq!(
        state.layout_preferences.saved.splits["history-body"],
        vec![0.2, 0.5, 0.3]
    );
}
