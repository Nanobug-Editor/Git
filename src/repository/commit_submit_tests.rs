//! Local commit ranges respond immediately and preserve bounded validation and commit/push ordering.

use super::*;

/// A real repository identity and draft make button availability depend only on index observation.
fn ready() -> State {
    let mut state = State::default();
    state.environment.os = "windows".into();
    state.repository.identity = Some(model::Identity {
        root: "C:/submit-test".into(),
        git_dir: "C:/submit-test/.git".into(),
        common_dir: "C:/submit-test/.git".into(),
    });
    state.commit_message = "selected change".into();
    let round = state.conflict_lifecycle.begin();
    state.conflict_lifecycle.status(round, Some(vec![]));
    let absent = conflicts::lifecycle::Marker::ALL
        .iter()
        .map(|marker| format!("{} missing\n", marker.reference()))
        .collect::<String>();
    state
        .conflict_lifecycle
        .probes(round, "", absent.as_bytes(), 0);
    state
}

/// Deliver complete process bytes through the production completion dispatcher.
fn receipt(command: Command, output: Vec<u8>) -> Running {
    Running {
        handle: api::ResourceHandle {
            instance: "submit-test".into(),
            scope: "workspace".into(),
            resource: 1,
        },
        command,
        output,
        stderr: Vec::new(),
    }
}

/// The checkbox receipt itself enables both actions; unversioned selection must not wait for Git.
#[test]
fn commit_range_untracked_selection_enables_actions_without_index_writes() {
    let mut state = ready();
    state.changes = vec![Change {
        path: "new/file.txt".into(),
        index: '.',
        worktree: '?',
        ..Default::default()
    }];
    state.tree_paths.insert(
        "staging:new".into(),
        trees::TreePath::Directory("untracked/new".into()),
    );
    assert!(state.commit_check("new", true).unwrap());
    assert!(state.queue.is_empty());
    assert!(state.editor_tasks.is_empty());
    for node in ["commit", "commit-push"] {
        assert!(
            ui::Document::new(state.commit_panel())
                .active_node(node)
                .is_some(),
            "{node} must respond to the local selection receipt"
        );
    }
}

/// Excluding a tracked file from the range must preserve an index prepared by another Git client.
#[test]
fn commit_range_tracked_uncheck_keeps_index_and_has_no_native_requests() {
    let mut state = ready();
    state.commit_selected.insert("file.txt".into());
    state.changes = vec![Change {
        path: "file.txt".into(),
        index: 'M',
        worktree: '.',
        ..Default::default()
    }];
    state.tree_paths.insert(
        "staging:file".into(),
        trees::TreePath::Leaf("changes/file.txt".into()),
    );
    state.commit_check("file", false).unwrap();
    assert!(state.queue.is_empty(), "a range toggle queued a Git write");
    assert!(state.editor_tasks.is_empty());
    assert_eq!(state.changes[0].index, 'M');
}

/// HEAD/branch changes after acceptance must cancel submission even when the file status is identical.
#[test]
fn commit_range_revalidates_head_and_branch_before_any_write() {
    let status = format!(
        "1 .M N... 100644 100644 100644 {} {} file.txt\0",
        "a".repeat(40),
        "a".repeat(40)
    );
    for (oid, branch) in [("b".repeat(40), "main"), ("a".repeat(40), "another")] {
        let mut state = ready();
        state.changes = crate::model::status(status.as_bytes()).unwrap();
        state.commit_selected.insert("file.txt".into());
        state.head = Some("a".repeat(40));
        state.current_ref = "refs/heads/main".into();
        state.commit_submitting = true;
        let (_, guard) = state.commit_submission_command(true).unwrap();
        let output = format!("# branch.oid {oid}\0# branch.head {branch}\0{status}").into_bytes();
        state.finished(receipt(guard, output), 0).unwrap();
        assert!(!state.commit_submitting);
        assert!(state.commit_submit_error.is_some());
        assert!(
            state
                .queue
                .iter()
                .all(|command| !matches!(command.args[0].as_str(), "add" | "commit" | "push"))
        );
        assert!(state.commit_selected.contains("file.txt"));
    }
}

/// Windows process/worker roundtrips cannot multiply by the number of operation marker files.
#[test]
fn explicit_stage_refresh_uses_two_reads_without_changing_the_local_range() {
    let mut state = ready();
    state.commit_selected.insert("file.txt".into());
    state
        .finished(
            receipt(
                Command::new(["add", "--", "file.txt"], Purpose::Action),
                Vec::new(),
            ),
            0,
        )
        .unwrap();
    assert!(
        ui::Document::new(state.commit_panel())
            .active_node("commit")
            .is_none()
    );
    let mut reads = 0;
    while let Some(command) = state.queue.pop_front() {
        reads += 1;
        let (bytes, code) = match command.purpose {
            Purpose::Status { .. } => (
                format!(
                    "1 M. N... 100644 100644 100644 {} {} file.txt\0",
                    "a".repeat(40),
                    "b".repeat(40)
                )
                .into_bytes(),
                0,
            ),
            Purpose::OperationBatch { .. } => (
                conflicts::lifecycle::Marker::ALL
                    .iter()
                    .map(|marker| format!("{} missing\n", marker.reference()))
                    .collect::<String>()
                    .into_bytes(),
                0,
            ),
            _ => panic!("unexpected post-stage command: {:?}", command.args),
        };
        state.finished(receipt(command, bytes), code).unwrap();
    }
    assert!(
        ui::Document::new(state.commit_panel())
            .active_node("commit")
            .is_some()
    );
    eprintln!("post-stage Git process completions before Commit is enabled: {reads}");
    assert!(
        reads <= 2,
        "Commit waited for {reads} serialized Git reads after the index write"
    );
}

/// The composer stays responsive to checks, but neither submit action may race an unfinished write.
#[test]
fn commit_buttons_use_local_range_and_share_validation() {
    let mut state = ready();
    state.commit_selected.insert("file.txt".into());
    state.changes = vec![Change {
        path: "file.txt".into(),
        index: 'M',
        worktree: '.',
        ..Default::default()
    }];
    for node in ["commit", "commit-push"] {
        assert!(
            ui::Document::new(state.commit_panel())
                .active_node(node)
                .is_some(),
            "{node}"
        );
    }
    state.commit_submitting = true;
    for node in ["commit", "commit-push"] {
        assert!(
            ui::Document::new(state.commit_panel())
                .active_node(node)
                .is_none(),
            "{node}"
        );
    }
    state.commit_submitting = false;
    state.commit_message.clear();
    assert!(
        ui::Document::new(state.commit_panel())
            .active_node("commit-push")
            .is_none()
    );
    state.commit_message = "draft".into();
    state.changes[0].conflict = true;
    assert!(
        ui::Document::new(state.commit_panel())
            .active_node("commit-push")
            .is_none()
    );
    state.changes[0].conflict = false;
    let round = state.conflict_lifecycle.begin();
    state
        .conflict_lifecycle
        .status(round, Some(state.changes.clone()));
    state.conflict_lifecycle.probes(round, "", b"broken", 0);
    assert!(
        ui::Document::new(state.commit_panel())
            .active_node("commit-push")
            .is_none()
    );
}

/// An unsupported ref backend keeps the same atomic observation and resolves every marker with Git.
#[test]
fn operation_batch_unsupported_storage_falls_back_without_publishing_absence() {
    let mut state = ready();
    let round = state.conflict_lifecycle.begin();
    state.conflict_lifecycle.status(round, Some(vec![]));
    state
        .finished(
            receipt(
                Command::new(["observe-operations"], Purpose::OperationBatch { round }),
                vec![],
            ),
            operation_observation::UNSUPPORTED_STORAGE_EXIT,
        )
        .unwrap();
    assert!(!state.conflict_lifecycle.ready());
    assert_eq!(state.queue.len(), 8);
    while let Some(command) = state.queue.pop_front() {
        assert_eq!(&command.args[..3], ["rev-parse", "--verify", "--quiet"]);
        state.finished(receipt(command, vec![]), 1).unwrap();
    }
    assert!(state.conflict_lifecycle.ready());
}

/// Route actual UI intent: only successful commit receipts may schedule a plain, non-force push.
#[test]
fn commit_push_sequences_success_and_retains_local_commit_on_push_failure() {
    for commit_code in [0, 1] {
        let mut state = ready();
        state.changes = vec![Change {
            path: "file.txt".into(),
            index: 'M',
            worktree: '.',
            ..Default::default()
        }];
        state.commit_selected.insert("file.txt".into());
        let (_, command) = state.commit_submission_command(true).unwrap();
        let Purpose::CommitSelection { next, .. } = command.purpose else {
            panic!("missing selection guard")
        };
        let command = *next;
        assert_eq!(command.args[0], "commit");
        assert!(command.args.contains(&"--only".into()));
        assert!(command.args.contains(&"selected change".into()));
        assert_eq!(command.input.as_deref(), Some(b"file.txt\0".as_slice()));
        state
            .finished(receipt(command, Vec::new()), commit_code)
            .unwrap();
        if commit_code == 0 {
            let push = state
                .queue
                .pop_front()
                .expect("push follows successful commit");
            assert_eq!(push.args, ["push"]);
            assert!(push.feedback_owner() == panel_activity::FeedbackOwner::Staging);
            let mut failed = receipt(push, Vec::new());
            failed.stderr = b"remote rejected".to_vec();
            state.finished(failed, 1).unwrap();
            assert!(
                state.message.contains("提交已成功") && state.message.contains("推送失败"),
                "{}",
                state.message
            );
        }
        assert!(
            state
                .queue
                .iter()
                .all(|next| !matches!(next.args[0].as_str(), "push" | "commit"))
        );
        assert_eq!(state.commit_message, "selected change");
    }
}
