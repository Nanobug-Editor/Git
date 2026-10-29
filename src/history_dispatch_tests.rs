//! Real dispatch receipts must finish the selected preview even after silent startup observations.
use super::*;

/// Keep the fixture at the public serialized guest boundary, including publication filtering.
fn dispatch(event: api::Notification) -> api::Output {
    dispatch_using(event, Command::launch)
}

/// Replacement native transport still crosses the same serialized input/output boundary.
fn dispatch_using(
    event: api::Notification,
    launch: impl FnMut(&Command, &str) -> Result<Running, api::Failure>,
) -> api::Output {
    let invocation = api::Invocation {
        id: 1,
        message: api::Input::Event {
            panel: Some("history".into()),
            event,
        },
    };
    let completion: api::Completion = serde_json::from_str(
        &dispatch_with(
            serde_json::to_string(&invocation).unwrap(),
            launch,
            feedback::accept,
        )
        .unwrap(),
    )
    .unwrap();
    completion.result.unwrap()
}

/// Metadata is already decoded here; actual-package tests cover its resource admission separately.
fn loading_state() -> State {
    let commit = Commit {
        oid: "a".repeat(40),
        title: "Selected commit".into(),
        ..Default::default()
    };
    let mut state = State {
        selected: commit.oid.clone(),
        history_selection: Some(commit.clone()),
        history_cursor: commit.oid.clone(),
        history_anchor: Some(commit.oid.clone()),
        selected_commits: vec![commit.oid.clone()],
        commits: vec![commit],
        history_loading: true,
        automatic_observation: true,
        background_read: true,
        ..Default::default()
    };
    // No native handle avoids invoking the tree host here; complete decoded metadata still
    // passes through production admission and the emitted files control retains its loading guard.
    state.pending_history_files = Some(BTreeMap::from([(
        "selected.txt".into(),
        model::HistoryChange::Modified,
    )]));
    let views = state.views();
    state.changed_views(views);
    state
}

/// Focus while loading requests another observation; that next silence cannot swallow this ready.
#[test]
fn metadata_ready_dispatch_survives_a_resumed_passive_observation() {
    let handle = fixture_handle(4);
    STATE.with(|cell| {
        let mut state = loading_state();
        state.passive_refresh_pending = true;
        state.running = Some(Running {
            handle: handle.clone(),
            command: Command::new(["show"], Purpose::CommitMessage(state.selected.clone())),
            output: b"Selected commit\n".to_vec(),
            stderr: Vec::new(),
        });
        *cell.borrow_mut() = state;
    });
    let mut launched = Vec::new();
    let output = dispatch_using(
        api::Notification::Process {
            handle,
            update: plugin_protocol::process::Update::Exited { code: 0 },
        },
        |command, _| {
            launched.push(command.args.clone());
            Ok(Running {
                handle: fixture_handle(5),
                command: command.clone(),
                output: Vec::new(),
                stderr: Vec::new(),
            })
        },
    );
    let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert_eq!(
        launched.len(),
        1,
        "the deferred observation starts normally"
    );
    assert!(state.running.is_some() && state.automatic_observation && state.background_read);
    assert!(!state.history_loading);
    let history = output
        .views
        .iter()
        .find(|view| view.panel == "history")
        .expect("next observation silence must not swallow selected-preview readiness");
    history.document.root.visit(&mut |node| {
        assert_ne!(node.id, "history-loading");
        if node.id == "files" {
            assert!(!node.disabled);
        }
    });
    assert_eq!(
        output.views.len(),
        1,
        "ready receipt keeps sibling scenes unchanged"
    );
}

/// A→B→A may make an old message OID match again, but its cancelled file map has no authority.
#[test]
fn delayed_same_oid_message_dispatch_cannot_enable_an_unmatched_file_tree() {
    let handle = fixture_handle(6);
    STATE.with(|cell| {
        let mut state = loading_state();
        state.background_read = false;
        state.automatic_observation = false;
        state.history_files = vec!["older-tree.txt".into()];
        state.running = Some(Running {
            handle: handle.clone(),
            command: Command::new(["show"], Purpose::CommitMessage(state.selected.clone())),
            output: b"Selected commit\n".to_vec(),
            stderr: Vec::new(),
        });
        state.commits.push(Commit {
            oid: "b".repeat(40),
            ..Default::default()
        });
        *cell.borrow_mut() = state;
    });
    let mut next = repeated_row();
    if let api::Notification::Ui(ui::UiEvent {
        action: ui::Action::Canvas(ui::CanvasEvent::Pointer { y, .. }),
        ..
    }) = &mut next
    {
        *y = 42.;
    }
    dispatch(next);
    dispatch(repeated_row());
    let output = dispatch_using(
        api::Notification::Process {
            handle,
            update: plugin_protocol::process::Update::Exited { code: 0 },
        },
        |command, _| {
            Ok(Running {
                handle: fixture_handle(7),
                command: command.clone(),
                output: Vec::new(),
                stderr: Vec::new(),
            })
        },
    );
    let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert!(
        state.history_loading,
        "matching OID alone cannot publish an old tree as ready"
    );
    assert_eq!(state.history_files, ["older-tree.txt"]);
    assert!(matches!(
        state.running.unwrap().command.purpose,
        Purpose::Files(_)
    ));
    for history in output.views.iter().filter(|view| view.panel == "history") {
        history.document.root.visit(&mut |node| {
            if node.id == "files" {
                assert!(node.disabled);
            }
        });
    }
}

/// Synthetic owned process identities only replace transport, never document or tree capabilities.
fn fixture_handle(resource: u64) -> api::ResourceHandle {
    api::ResourceHandle {
        instance: "history-dispatch".into(),
        scope: "workspace".into(),
        resource,
    }
}

/// The final process receipt itself must remove loading and re-enable files, without a later input.
#[test]
fn startup_metadata_completion_dispatch_publishes_ready_history() {
    let handle = api::ResourceHandle {
        instance: "history-dispatch".into(),
        scope: "workspace".into(),
        resource: 1,
    };
    STATE.with(|cell| {
        let mut state = loading_state();
        state.running = Some(Running {
            handle: handle.clone(),
            command: Command::new(["show"], Purpose::CommitMessage(state.selected.clone())),
            output: b"Selected commit\n".to_vec(),
            stderr: Vec::new(),
        });
        *cell.borrow_mut() = state;
    });
    let output = dispatch(api::Notification::Process {
        handle,
        update: plugin_protocol::process::Update::Exited { code: 0 },
    });
    let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert!(!state.history_loading && !state.busy());
    let history = output
        .views
        .iter()
        .find(|view| view.panel == "history")
        .expect("the final metadata receipt must publish its ready scene");
    let mut files_enabled = false;
    history.document.root.visit(&mut |node| {
        assert_ne!(node.id, "history-loading");
        if node.id == "files" {
            files_enabled = !node.disabled;
        }
    });
    assert!(
        files_enabled,
        "the retained native tree becomes clickable immediately"
    );
    assert_eq!(
        output.views.len(),
        1,
        "metadata cannot refresh the composer"
    );
}

/// A repeated row gesture cannot cancel required metadata merely because startup queued it.
#[test]
fn repeated_history_dispatch_preserves_current_metadata_and_drops_optional_reads() {
    STATE.with(|cell| {
        let mut state = loading_state();
        state.running = Some(Running {
            handle: api::ResourceHandle {
                instance: "history-dispatch".into(),
                scope: "workspace".into(),
                resource: 2,
            },
            command: Command::new(["show"], Purpose::CommitMessage(state.selected.clone())),
            output: Vec::new(),
            stderr: Vec::new(),
        });
        state.queue.push_back(Command::new(["refs"], Purpose::Refs));
        *cell.borrow_mut() = state;
    });
    dispatch(repeated_row());
    let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert!(state.history_loading);
    assert!(matches!(
        state.running.unwrap().command.purpose,
        Purpose::CommitMessage(_)
    ));
    assert!(
        state.queue.is_empty(),
        "optional repository reads yield to current input"
    );
    assert!(!state.background_read && !state.automatic_observation);
}

/// A previous cancellation may leave loading with no owner; the same row must recover that demand.
#[test]
fn repeated_history_dispatch_recovers_metadata_without_a_live_query() {
    STATE.with(|cell| {
        let mut state = loading_state();
        state.background_read = false;
        state.automatic_observation = false;
        // Another read retains the process slot, making the newly required metadata observable
        // in the queue without invoking a native process transport in this unit test.
        state.running = Some(Running {
            handle: api::ResourceHandle {
                instance: "history-dispatch".into(),
                scope: "workspace".into(),
                resource: 3,
            },
            command: Command::new(["refs"], Purpose::Refs),
            output: Vec::new(),
            stderr: Vec::new(),
        });
        *cell.borrow_mut() = state;
    });
    dispatch(repeated_row());
    let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert_eq!(
        state.queue.len(),
        1,
        "an orphaned loading state must recover its read"
    );
    assert!(matches!(&state.queue[0].purpose, Purpose::Files(oid) if oid == &state.selected));
}

/// The click is on the already selected row, away from the native column separators.
fn repeated_row() -> api::Notification {
    api::Notification::Ui(ui::UiEvent {
        revision: 1,
        node: "log".into(),
        action: ui::Action::Canvas(ui::CanvasEvent::Pointer {
            phase: ui::PointerPhase::Down,
            x: 100.,
            y: 14.,
            button: 0,
            clicks: 1,
            shift: false,
        }),
    })
}
