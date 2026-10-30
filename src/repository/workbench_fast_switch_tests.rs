//! Branch-switch latency and authority regressions use the production dispatcher/completion seam.
use super::*;
use crate::model::HistoryChange;

/// Loaded all-ref history can immediately project known ancestry while Git fills a bounded page.
#[test]
fn branch_switch_projects_known_ancestry_and_keeps_every_control_usable() {
    let repository = commits();
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &repository);
    state
        .ref_tips
        .insert("refs/heads/topic".into(), repository[20].oid.clone());
    state
        .select_history_filter(Some("refs/heads/topic".into()))
        .unwrap();
    assert_eq!(state.commits[0].oid, repository[20].oid);
    assert_eq!(state.commits.len(), 108);
    assert!(state.history_cache.pending);
    assert!(
        !state.ui_busy(),
        "readonly filtering must not disable review/staging input"
    );
    let views = state.views();
    let history = &views
        .iter()
        .find(|view| view.panel == "history")
        .unwrap()
        .document;
    assert!(history.active_node("log").is_some());
    let Some(ui::Kind::Canvas(canvas)) = history.active_node("log").map(|node| &node.kind) else {
        panic!("mounted canvas")
    };
    assert!(canvas.paint.iter().any(
        |paint| matches!(paint, plugin_protocol::Paint::Text { text, .. } | plugin_protocol::Paint::TextBox { text, .. } if text == "Commit 20")
    ));
    assert_eq!(
        state
            .queue
            .iter()
            .find(|command| matches!(command.purpose, Purpose::Log { .. }))
            .unwrap()
            .args
            .iter()
            .find(|arg| arg.starts_with("--max-count="))
            .unwrap(),
        "--max-count=128"
    );
}

/// A superseded success or error cannot install another filter's page or clear its pending queue.
#[test]
fn branch_switch_rejects_superseded_success_error_and_aba_receipts() {
    for exit in [0, 128] {
        let mut state = workbench();
        state
            .select_history_filter(Some("refs/heads/a".into()))
            .unwrap();
        let retired = state.queue.pop_front().unwrap();
        state
            .select_history_filter(Some("refs/heads/b".into()))
            .unwrap();
        state
            .select_history_filter(Some("refs/heads/a".into()))
            .unwrap();
        let queued = state.queue.len();
        state
            .finished(
                Running {
                    handle: handle(),
                    command: retired,
                    output: Vec::new(),
                    stderr: b"old error".to_vec(),
                },
                exit,
            )
            .unwrap();
        assert_eq!(state.queue.len(), queued);
        assert!(state.message.is_empty());
        assert!(state.history_cache.pending);
    }
}

/// Moving an OID behind the same branch name invalidates cached reachability and decorations.
#[test]
fn branch_switch_ref_target_changes_invalidate_logs_without_reloading_branch_names() {
    let repository = commits();
    let mut state = workbench();
    let finish_refs = |state: &mut State, tip: &str| {
        state
            .finished(
                Running {
                    handle: handle(),
                    command: Command::new(["refs"], Purpose::Refs),
                    output: format!("*\trefs/heads/main\t{tip}\n").into_bytes(),
                    stderr: Vec::new(),
                },
                0,
            )
            .unwrap();
    };
    finish_refs(&mut state, &repository[0].oid);
    state
        .select_history_filter(Some("refs/heads/main".into()))
        .unwrap();
    complete_history(&mut state, &repository[..10]);
    state.select_history_filter(None).unwrap();
    complete_history(&mut state, &repository[..10]);
    finish_refs(&mut state, &repository[1].oid);
    state
        .select_history_filter(Some("refs/heads/main".into()))
        .unwrap();
    assert!(
        state
            .queue
            .iter()
            .any(|command| matches!(command.purpose, Purpose::Log { .. }))
    );
    complete_history(&mut state, &repository[1..10]);
    assert_eq!(state.commits[0].oid, repository[1].oid);
}

/// Immutable preview caching removes the two serial child processes from a warm branch switch.
#[test]
fn branch_switch_restores_complete_preview_without_any_git_commands() {
    let repository = commits();
    let mut state = workbench();
    for (branch, rows) in [("a", &repository[..10]), ("b", &repository[10..20])] {
        state
            .select_history_filter(Some(format!("refs/heads/{branch}")))
            .unwrap();
        complete_history(&mut state, rows);
        state.history_preview_commit = state.history_selection.clone();
        state.history_file_status =
            BTreeMap::from([(format!("{branch}.txt"), HistoryChange::Added)]);
        state.history_files = vec![format!("{branch}.txt")];
        state.history_message = format!("{branch} complete message");
        state.history_loading = false;
        state.cache_history_preview();
    }
    let started = std::time::Instant::now();
    state
        .select_history_filter(Some("refs/heads/a".into()))
        .unwrap();
    assert!(state.queue.is_empty());
    assert_eq!(state.history_files, ["a.txt"]);
    assert_eq!(state.history_message, "a complete message");
    assert_eq!(
        state.history_preview_commit.as_ref().unwrap().oid,
        repository[0].oid
    );
    assert!(!state.history_loading);
    eprintln!(
        "cached log + complete preview: {:?}, child queries=0",
        started.elapsed()
    );
}

/// A write remains serialized and disabled; cache preemption can only cancel pure history reads.
#[test]
fn branch_switch_cache_cannot_cancel_or_unlock_repository_writes() {
    let mut state = workbench();
    state
        .queue
        .push_back(Command::new(["commit", "-m", "write"], Purpose::Action));
    state.cancel_history_reads().unwrap();
    assert_eq!(state.queue.len(), 1);
    assert!(state.ui_busy());
    assert!(!state.history_reads_only());
}

/// A cancelled cold query cannot reuse another branch's displayed row count as its Git skip.
#[test]
fn branch_switch_cancelled_cold_filter_restarts_pagination_from_zero() {
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &commits());
    state
        .select_history_filter(Some("refs/heads/cold".into()))
        .unwrap();
    state.cancel_history_reads().unwrap();
    state
        .ui_event(
            Some("history"),
            ui::UiEvent {
                revision: state.revision,
                node: "more".into(),
                action: ui::Action::Click,
            },
        )
        .unwrap();
    assert_eq!(
        state
            .queue
            .front()
            .unwrap()
            .args
            .iter()
            .find(|arg| arg.starts_with("--skip="))
            .unwrap(),
        "--skip=0"
    );
}

/// Reading an old visible commit may preempt a cold filter, which must resume once that action ends.
#[test]
fn branch_switch_cold_filter_resumes_after_a_preempting_preview() {
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &commits());
    state
        .select_history_filter(Some("refs/heads/cold".into()))
        .unwrap();
    state.cancel_history_reads().unwrap();
    state.select_history_row(4, false);
    state.resume_pending_history_filter();
    assert!(
        !state
            .queue
            .iter()
            .any(|command| matches!(command.purpose, Purpose::Log { .. }))
    );
    complete_history(&mut state, &commits());
    state.resume_pending_history_filter();
    assert!(
        matches!(state.queue.front().unwrap().purpose, Purpose::Log { ref filter, .. } if filter.as_deref()==Some("refs/heads/cold"))
    );
}
