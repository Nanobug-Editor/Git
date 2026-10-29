//! Optional reads are exercised through ordinary process completion, queueing and guest dispatch.
use super::*;

/// One accepted middle file has a frozen next/previous list, independent of the selected log row.
fn fixture() -> State {
    let mut state = State {
        diff_key: "retained".into(),
        selected: "moving selection".into(),
        selected_path: "selected.rs".into(),
        history_path: "shown.rs".into(),
        columns: vec!["visible left".into(), "visible right".into()],
        snapshot_targets: vec!["moving parent".into(), "moving commit".into()],
        message: "unchanged diagnostic".into(),
        history_error: "unchanged history diagnostic".into(),
        ..Default::default()
    };
    state.environment.workspace = "repo".into();
    state.diff_views.insert(
        "retained".into(),
        DiffSources {
            navigation: HistoryNavigation::capture(
                vec!["a".repeat(40), "b".repeat(40)],
                vec![
                    "previous.rs".into(),
                    "current.rs".into(),
                    "next\nfile.rs".into(),
                    "outside.rs".into(),
                ],
                "current.rs",
            ),
            ..Default::default()
        },
    );
    state.schedule_history_prefetch();
    state
}

/// Exact owned handles distinguish retired updates from the following real operation.
fn handle(resource: u64) -> api::ResourceHandle {
    api::ResourceHandle {
        instance: "prefetch-test".into(),
        scope: "workspace".into(),
        resource,
    }
}

/// The fake process boundary preserves the real command's immutable purpose and literal stdin.
fn running(command: &Command) -> Running {
    Running {
        handle: handle(1),
        command: command.clone(),
        output: Vec::new(),
        stderr: Vec::new(),
    }
}

/// The existing complete batch decoder receives both a real blob and confirmed missing side.
fn response(request: &HistoryPrefetch) -> Vec<u8> {
    request
        .targets
        .iter()
        .enumerate()
        .flat_map(|(index, oid)| {
            if index == 0 {
                format!("{} blob 6\nhello\n\n", "c".repeat(40)).into_bytes()
            } else {
                format!("{oid}:{} missing\n", request.path).into_bytes()
            }
        })
        .collect()
}

/// Start one speculative item using the production idle gate and pump.
fn start(state: &mut State) -> HistoryPrefetch {
    state.resume_history_prefetch();
    assert!(!state.pump_with(|command, cwd| {
        assert_eq!(cwd, "repo");
        Ok(running(command))
    }));
    let Purpose::HistoryPrefetch(request) = &state.running.as_ref().unwrap().command.purpose else {
        panic!()
    };
    request.clone()
}

/// Only immediate neighbors and exact OIDs are admitted; empty parents do not start Git reads.
#[test]
fn prefetch_is_bounded_to_missing_fixed_commit_neighbors() {
    let mut state = fixture();
    assert_eq!(state.history_prefetch.len(), 2);
    assert_eq!(state.history_prefetch[0].path, "next\nfile.rs");
    assert_eq!(state.history_prefetch[1].path, "previous.rs");
    state
        .history_sources
        .remember("repo", &"a".repeat(40), "next\nfile.rs", Some("cached"));
    let request = start(&mut state);
    assert_eq!(request.targets, ["b".repeat(40)]);
    assert_eq!(
        state
            .running
            .as_ref()
            .unwrap()
            .command
            .input
            .as_ref()
            .unwrap(),
        format!("{}:next\nfile.rs\0", "b".repeat(40)).as_bytes()
    );
    assert!(!state.ui_busy());
    assert!(state.editor_tasks.is_empty());
    state.running = None;
    for targets in [
        vec!["".into(), "b".repeat(40)],
        vec!["HEAD".into(), "b".repeat(40)],
        vec![":0".into(), "b".repeat(40)],
    ] {
        let immutable = targets[0].is_empty();
        state.diff_views.get_mut("retained").unwrap().navigation =
            HistoryNavigation::capture(targets, vec!["a".into(), "b".into()], "a");
        state.schedule_history_prefetch();
        assert_eq!(state.history_prefetch.len(), usize::from(immutable));
        if immutable {
            assert_eq!(state.history_prefetch[0].targets, ["b".repeat(40)]);
        }
    }
}

/// A required write or pending comparison is served before a speculative read.
#[test]
fn prefetch_yields_to_required_queue_and_pending_selection() {
    let mut state = fixture();
    state
        .queue
        .push_back(Command::new(["commit"], Purpose::Action));
    state.resume_history_prefetch();
    assert_eq!(state.queue.len(), 1);
    assert_eq!(state.history_prefetch.len(), 2);
    state.queue.clear();
    state
        .pending_comparisons
        .push_back(panel_activity::ComparisonSelection::History {
            oid: "wanted".into(),
            path: "file".into(),
        });
    state.resume_history_prefetch();
    assert!(state.queue.is_empty());
    state.pending_comparisons.clear();
    state.background_read = true;
    state.automatic_observation = true;
    let _ = start(&mut state);
    assert!(!state.background_read && !state.automatic_observation);
    assert!(
        state
            .cancel_history_prefetch(|actual| {
                assert_eq!(actual, handle(1));
                Ok(())
            })
            .is_ok()
    );
    state
        .queue
        .push_back(Command::new(["commit"], Purpose::Action));
    state.resume_history_prefetch();
    assert!(state.history_prefetch.is_empty());
    state.pump_with(|command, _| {
        assert_eq!(command.args, ["commit"]);
        Ok(running(command))
    });
}

/// Speculation caches captured identities only and does not modify the active file or native sources.
#[test]
fn prefetch_completion_only_warms_captured_sources() {
    let mut state = fixture();
    let request = start(&mut state);
    let mut process = state.running.take().unwrap();
    process.output = response(&request);
    state.finished(process, 0).unwrap();
    assert_eq!(
        state
            .history_sources
            .get("repo", &"a".repeat(40), "next\nfile.rs"),
        Some(Some("hello\n".into()))
    );
    assert_eq!(
        state
            .history_sources
            .get("repo", &"b".repeat(40), "next\nfile.rs"),
        Some(None)
    );
    assert!(
        !state
            .history_sources
            .contains("repo", "moving commit", "next\nfile.rs")
    );
    assert_eq!(state.selected, "moving selection");
    assert_eq!(state.selected_path, "selected.rs");
    assert_eq!(state.history_path, "shown.rs");
    assert_eq!(state.columns, ["visible left", "visible right"]);
    assert_eq!(state.snapshot_targets, ["moving parent", "moving commit"]);
    assert_eq!(state.snapshot_ready, 0);
    assert!(state.editor_tasks.is_empty());
    assert!(state.queue.is_empty());
    // The ordinary click consumes the prefetched cache without another process or API.
    state.snapshot_targets = vec!["a".repeat(40), "b".repeat(40)];
    state.queue_history_sources("next\nfile.rs");
    assert!(state.queue.is_empty());
    assert_eq!(state.columns, ["hello\n", ""]);
}

/// Failed, malformed, binary or oversized batches remain misses and cannot erase real work.
#[test]
fn prefetch_failures_are_silent_and_do_not_retry_the_consumed_item() {
    for (output, code) in [
        (Vec::new(), 1),
        (b"truncated".to_vec(), 0),
        (format!("{} blob 1\n\0\n", "c".repeat(40)).into_bytes(), 0),
        (
            format!(
                "{} blob {}\n{}\n",
                "c".repeat(40),
                256 * 1024 + 1,
                "x".repeat(256 * 1024 + 1)
            )
            .into_bytes(),
            0,
        ),
    ] {
        let mut state = fixture();
        let request = start(&mut state);
        state.history_prefetch.clear();
        state
            .queue
            .push_back(Command::new(["commit"], Purpose::Action));
        let mut process = state.running.take().unwrap();
        process.output = output;
        state.finished(process, code).unwrap();
        for _ in 0..3 {
            state.resume_history_prefetch();
        }
        assert!(
            !state
                .history_sources
                .contains("repo", &request.targets[0], &request.path)
        );
        assert_eq!(state.queue.len(), 1);
        assert_eq!(state.queue[0].args, ["commit"]);
        assert!(state.history_prefetch.is_empty());
        assert_eq!(state.message, "unchanged diagnostic");
        assert_eq!(state.history_error, "unchanged history diagnostic");
    }
}

/// A launch failure consumes only the optional item and leaves controls and diagnostics alone.
#[test]
fn prefetch_launch_failure_keeps_the_required_queue() {
    let mut state = fixture();
    state.resume_history_prefetch();
    state.history_prefetch.clear();
    state
        .queue
        .push_back(Command::new(["commit"], Purpose::Action));
    assert!(!state.pump_with(|_, _| Err(failure("launch failed"))));
    assert_eq!(state.queue.len(), 1);
    assert_eq!(state.queue[0].args, ["commit"]);
    assert_eq!(state.message, "unchanged diagnostic");
    assert_eq!(state.history_error, "unchanged history diagnostic");
}

/// Revoked origin/workspace ownership cannot write cache, whether waiting or already running.
#[test]
fn prefetch_rejects_retired_origin_and_workspace() {
    for workspace_changed in [false, true] {
        let mut state = fixture();
        let request = start(&mut state);
        if workspace_changed {
            state.environment.workspace = "another repo".into();
        } else {
            state.diff_views.clear();
        }
        let mut process = state.running.take().unwrap();
        process.output = response(&request);
        state.finished(process, 0).unwrap();
        state.resume_history_prefetch();
        assert!(state.queue.is_empty());
        assert!(state.history_prefetch.is_empty());
        assert!(
            !state
                .history_sources
                .contains("repo", &request.targets[0], &request.path)
        );
    }
}

/// Failed cancellation and stdin cleanup retain their exact resource until terminal observation.
#[test]
fn prefetch_failed_release_preserves_handle_but_revokes_cache_authority() {
    let mut state = fixture();
    let request = start(&mut state);
    assert!(
        state
            .cancel_history_prefetch(|_| Err(failure("close failed")))
            .is_err()
    );
    assert_eq!(state.running.as_ref().unwrap().handle, handle(1));
    let mut process = state.running.take().unwrap();
    process.output = response(&request);
    state.finished(process, 0).unwrap();
    assert!(
        !state
            .history_sources
            .contains("repo", &request.targets[0], &request.path)
    );
    let command = request.command();
    let retained = command
        .failed_launch(handle(9), failure("stdin failed"), |_| {
            Err(failure("close failed"))
        })
        .unwrap();
    assert_eq!(retained.handle, handle(9));
    assert!(matches!(
        retained.command.purpose,
        Purpose::HistoryPrefetch(HistoryPrefetch {
            discarded: true,
            ..
        })
    ));
}

/// Output caps and process termination discard only speculative work, including failed closes.
#[test]
fn prefetch_output_limit_and_termination_preserve_user_work() {
    let mut state = fixture();
    let request = start(&mut state);
    state
        .queue
        .push_back(Command::new(["commit"], Purpose::Action));
    state.history_prefetch_update(
        process::Update::Output {
            stream: process::Stream::Stdout,
            bytes: vec![b'x'; 1024 * 1024 + 1],
        },
        |_| Err(failure("close failed")),
    );
    assert_eq!(state.running.as_ref().unwrap().handle, handle(1));
    assert!(state.running.as_ref().unwrap().output.is_empty());
    state.history_prefetch_update(
        process::Update::Output {
            stream: process::Stream::Stdout,
            bytes: response(&request),
        },
        |_| panic!("must not retry automatically"),
    );
    assert!(state.running.as_ref().unwrap().output.is_empty());
    state.history_prefetch_update(process::Update::Terminated, |_| panic!());
    assert!(state.running.is_none());
    assert_eq!(state.queue[0].args, ["commit"]);
    assert_eq!(state.message, "unchanged diagnostic");
    assert_eq!(state.history_error, "unchanged history diagnostic");
}

/// The actual guest process receipt emits no UI updates or native source allocations.
#[test]
fn prefetch_dispatch_completion_is_invisible_and_stale_receipts_are_ignored() {
    let mut state = fixture();
    let request = start(&mut state);
    state.history_prefetch.clear();
    state.running.as_mut().unwrap().output = response(&request);
    STATE.with(|cell| *cell.borrow_mut() = state);
    let invocation = api::Invocation {
        id: 1,
        message: api::Input::Event {
            panel: None,
            event: api::Notification::Process {
                handle: handle(1),
                update: process::Update::Exited { code: 0 },
            },
        },
    };
    for _ in 0..2 {
        let completion: api::Completion = serde_json::from_str(
            &dispatch_with(serde_json::to_string(&invocation).unwrap(), |_, _| {
                panic!("unexpected query retry")
            })
            .unwrap(),
        )
        .unwrap();
        assert!(completion.result.unwrap().views.is_empty());
    }
    let state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    assert!(state.running.is_none());
    assert!(state.editor_tasks.is_empty());
    assert_eq!(state.selected_path, "selected.rs");
    assert!(
        state
            .history_sources
            .contains("repo", &request.targets[0], &request.path)
    );
}
