//! Native document receipts and UI actions exercise the guest's approved confirmation boundary.
//! Git replies are immutable observations; no test can save or edit a host-owned document.
use super::*;

/// A successful discovery identity keeps every fixture action scoped to one owned worktree.
fn workbench() -> State {
    State {
        repository: Repository {
            identity: Some(Identity {
                root: "C:/dirty-fixture".into(),
                git_dir: "C:/dirty-fixture/.git".into(),
                common_dir: "C:/dirty-fixture/.git".into(),
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn hash(character: char) -> String {
    character.to_string().repeat(40)
}

fn document(dirty: bool, revision: u64) -> api::DocumentInfo {
    api::DocumentInfo {
        document: api::DocumentVersion {
            id: "native-buffer".into(),
            path: "tracked/after-save.txt".into(),
            revision,
        },
        resource: api::ResourceIdentity::Local {
            path: "tracked/after-save.txt".into(),
        },
        title: "after-save.txt".into(),
        access: api::DocumentAccess {
            read: true,
            edit: true,
            save: true,
        },
        dirty,
        language: "text".into(),
        encoding: api::DocumentEncoding::Utf8,
        eol: api::DocumentEol::Lf,
        byte_len: 12,
        selection: api::TextRange { start: 0, end: 0 },
        visible_rows: None,
    }
}

fn click(state: &mut State, node: &str) -> Result<(), api::Failure> {
    state.ui_event(
        Some("history"),
        ui::UiEvent {
            revision: state.revision,
            node: node.into(),
            action: ui::Action::Click,
        },
    )
}

/// Consume only production read-only preview commands and return their complete fixture replies.
fn finish_preview(state: &mut State, untracked: &[u8]) {
    if state
        .queue
        .front()
        .is_some_and(|command| command.args.first().map(String::as_str) == Some("status"))
    {
        let command = state.queue.pop_front().unwrap();
        let Purpose::OperationPreview(query) = command.purpose else {
            panic!("saved worktree observation belongs to the preview")
        };
        state
            .operation_preview_finished(query, b"? tracked/after-save.txt\0", b"", 0)
            .unwrap();
    }
    for bytes in [
        b"refs/heads/main\n".to_vec(),
        format!("{}\n", hash('a')).into_bytes(),
        format!("{}\n", hash('b')).into_bytes(),
        format!("{}\0\0original commit\0", hash('a')).into_bytes(),
        Vec::new(),
        format!("100644 blob {}\ttracked\0", hash('c')).into_bytes(),
        untracked.to_vec(),
        b"refs/heads/main\n".to_vec(),
        format!("{}\n", hash('a')).into_bytes(),
    ] {
        let command = state.queue.pop_front().expect("next read-only query");
        let Purpose::OperationPreview(query) = command.purpose else {
            panic!("an impact review may not execute a Git mutation")
        };
        state
            .operation_preview_finished(query, &bytes, b"", 0)
            .unwrap();
    }
    assert!(state.queue.is_empty());
}

/// Receiving a dirty revision must revoke approval. Saving can create new hard-reset blockers,
/// so the next click must read them and publish a second review before any write is eligible.
#[test]
fn saving_after_dirty_guard_requires_fresh_impact_review_before_mutation() {
    let mut state = State::default();
    assert!(
        state
            .prepare_operation_preview(
                Command::new(["reset", "--hard", "topic"], Purpose::Action),
                "history".into(),
                false,
            )
            .unwrap()
    );
    finish_preview(&mut state, b"");
    assert!(
        !state
            .pending
            .as_ref()
            .unwrap()
            .preview()
            .contains("after-save.txt")
    );

    // Model the accepted ListDocuments response to the first explicit confirmation.
    state.write_intent = state.pending.take();
    let error = state
        .complete_write_check(&[document(true, 9)])
        .unwrap_err();
    assert!(error.to_string().contains("Save these buffers"));
    assert!(state.queue.is_empty());

    // A manual native save changes the disk inventory. UI selection changes cannot retarget
    // the operation while the old confirmation is being replaced with a fresh impact review.
    state.argument = "different-ui-target".into();
    click(&mut state, "confirm-command").unwrap();
    assert!(state.pending.is_none());
    assert!(state.write_intent.is_none());
    assert!(state.editor_tasks.is_empty());

    finish_preview(&mut state, b"tracked/after-save.txt\0");
    let fresh = state.pending.as_ref().expect("second explicit review");
    assert!(fresh.preview().contains("\"tracked/after-save.txt\""));
    assert!(!fresh.preview().contains("different-ui-target"));
    assert_eq!(fresh.effective_args().last(), Some(&hash('b')));
    assert!(
        state.queue.is_empty(),
        "new review never implies confirmation"
    );

    // Only the separately accepted second confirmation can release its command through a
    // fresh native-buffer observation; the native session and its revision remain unchanged.
    state.write_intent = state.pending.take();
    state.complete_write_check(&[document(false, 10)]).unwrap();
    let allowed = state
        .queue
        .pop_front()
        .expect("explicitly confirmed operation");
    assert!(allowed.preview().contains("\"tracked/after-save.txt\""));
    assert_eq!(
        allowed.effective_args().first().map(String::as_str),
        Some("reset")
    );
}

/// Operations without a complete ordinary-history preview must lose their old approval. Native
/// saving is never treated as permission to run a switch, discard, stash or resumed helper command.
#[test]
fn saved_buffers_require_reselecting_other_worktree_operations() {
    let mut helper = Command::new(["rebase", "--start", "base"], Purpose::Action);
    helper.service = Some("rebase".into());
    for (command, original_node) in [
        (Command::new(["switch", "topic"], Purpose::Action), "switch"),
        (
            Command::new(
                ["restore", "--worktree", "--", "tracked/after-save.txt"],
                Purpose::Action,
            ),
            "discard",
        ),
        (
            Command::new(["stash", "push", "--include-untracked"], Purpose::Action),
            "stash",
        ),
        (
            Command::new(["merge", "--continue"], Purpose::Action),
            "continue",
        ),
        (helper, "rebase-plan"),
    ] {
        let mut state = workbench();
        state.write_intent = Some(command);
        state.deferred_operation = Some(Command::new(["switch", "older"], Purpose::Action));
        state
            .complete_write_check(&[document(true, 9)])
            .unwrap_err();
        click(&mut state, "confirm-command").unwrap();

        assert!(
            state.pending.is_none(),
            "{original_node} kept an old approval"
        );
        assert!(state.write_intent.is_none());
        assert!(state.deferred_operation.is_none());
        assert!(
            state.editor_tasks.is_empty(),
            "a review cannot save or edit native text"
        );
        assert!(state.message.contains("重新选择操作"));
        while let Some(read) = state.queue.pop_front() {
            assert!(
                matches!(
                    read.purpose,
                    Purpose::Status { .. }
                        | Purpose::Refs
                        | Purpose::Head
                        | Purpose::Log { .. }
                        | Purpose::OperationProbe { .. }
                ),
                "{original_node} scheduled an unapproved mutation: {:?}",
                read.args
            );
        }
        assert!(
            !state.busy(),
            "completed read-only refresh must allow new actions"
        );

        // A new selection goes through its original UI path. Continue's first native request
        // belongs to the actual-package tests; its explicit recovery preview remains reachable.
        state.argument = "fresh-topic".into();
        state.selected_path = "tracked/after-save.txt".into();
        state.changes = vec![Change {
            path: state.selected_path.clone(),
            worktree: 'M',
            ..Default::default()
        }];
        if original_node == "continue" {
            state.operation = Some("merge".into());
            click(&mut state, "abort").unwrap();
            assert_eq!(
                state.pending.as_ref().unwrap().effective_args(),
                ["merge", "--abort"]
            );
        } else {
            click(&mut state, original_node).unwrap();
            match original_node {
                "switch" => assert_eq!(
                    state.pending.as_ref().unwrap().effective_args(),
                    ["switch", "fresh-topic"]
                ),
                "discard" => assert_eq!(
                    state.pending.as_ref().unwrap().effective_args(),
                    ["restore", "--worktree", "--", "tracked/after-save.txt"]
                ),
                _ => assert!(
                    !state.queue.is_empty(),
                    "the original read-only preparation must restart"
                ),
            }
        }
    }
}

/// Cancellation is accepted while native enumeration is in flight. The revoked request retains
/// only a cleanup correlation; it cannot hold the UI busy or claim a subsequently selected action.
#[test]
fn cancelled_buffer_check_releases_confirmation_and_allows_new_action() {
    let mut state = workbench();
    state.write_intent = Some(Command::new(["switch", "older-topic"], Purpose::Action));
    state.editor_tasks.insert(41, "write-check".into());
    click(&mut state, "cancel-command").unwrap();

    assert!(
        state.write_intent.is_none(),
        "cancel must revoke the accepted write intent"
    );
    assert!(state.pending.is_none());
    assert_eq!(
        state.editor_tasks.get(&41).map(String::as_str),
        Some("write-check-cancelled")
    );
    assert!(
        !state.busy(),
        "cancelled enumeration cannot block the new selection"
    );

    state.argument = "new-topic".into();
    click(&mut state, "switch").unwrap();
    assert_eq!(
        state.pending.as_ref().unwrap().effective_args(),
        ["switch", "new-topic"]
    );

    // The new accepted request owns a different correlation. The old request remains classified
    // solely for the production Request Completed path, which closes its resource then ignores it.
    state.write_intent = state.pending.take();
    state.editor_tasks.insert(42, "write-check".into());
    assert_eq!(
        state.editor_tasks.get(&41).map(String::as_str),
        Some("write-check-cancelled")
    );
    assert_eq!(
        state.editor_tasks.get(&42).map(String::as_str),
        Some("write-check")
    );
    assert_eq!(
        state.write_intent.as_ref().unwrap().effective_args(),
        ["switch", "new-topic"]
    );
    assert!(state.queue.is_empty());
}

/// A manual save can make Git's formerly clean worktree dirty. Rebuilt merge approval must expose
/// the explicit commit/stash/cancel choice rather than silently carrying those newly saved bytes.
#[test]
fn saving_before_merge_rechecks_git_changes_before_publishing_new_confirmation() {
    let mut state = workbench();
    state.write_intent = Some(Command::new(
        ["merge", "--no-autostash", "--no-edit", "--", &hash('b')],
        Purpose::Action,
    ));
    state
        .complete_write_check(&[document(true, 9)])
        .unwrap_err();
    click(&mut state, "confirm-command").unwrap();

    while let Some(command) = state.queue.pop_front() {
        let bytes = match command.args.first().map(String::as_str) {
            Some("status") => b"? tracked/after-save.txt\0".to_vec(),
            Some("rev-parse")
                if command.args.get(1).map(String::as_str) == Some("--symbolic-full-name") =>
            {
                b"refs/heads/main\n".to_vec()
            }
            Some("rev-parse") if command.args.last().map(String::as_str) == Some("HEAD") => {
                format!("{}\n", hash('a')).into_bytes()
            }
            Some("rev-parse") => format!("{}\n", hash('b')).into_bytes(),
            Some("-c") => format!("{}\0\0incoming commit\0", hash('b')).into_bytes(),
            Some("for-each-ref") => Vec::new(),
            _ => panic!(
                "only bounded read-only merge queries are allowed: {:?}",
                command.args
            ),
        };
        let Purpose::OperationPreview(query) = command.purpose else {
            panic!("reconsideration must remain a read-only preview")
        };
        state
            .operation_preview_finished(query, &bytes, b"", 0)
            .unwrap();
    }
    assert!(state.pending.is_some());
    assert!(
        state.pending_dirty,
        "newly saved Git changes need an explicit dirty-worktree choice"
    );
    assert!(
        state
            .changes
            .iter()
            .any(|change| change.path == "tracked/after-save.txt")
    );
    assert!(state.write_intent.is_none());
    assert!(state.editor_tasks.is_empty());
}

/// A persisted start plan is an approval, while a running rebase's plan is required for recovery.
/// Cancelling a blocked start must not let a later action or plugin restart resurrect that approval.
#[test]
fn cancelling_a_blocked_rebase_revokes_only_the_unstarted_plan() {
    for running in [false, true] {
        let mut state = workbench();
        state.operation = running.then(|| "rebase".into());
        state.interactive_plan = Some(rebase_model::Plan {
            workspace: "C:/dirty-fixture".into(),
            git: "git".into(),
            base: hash('b'),
            head: hash('a'),
            branch: Some("refs/heads/main".into()),
            original: vec![hash('a')],
            steps: vec![rebase_model::Step {
                oid: hash('a'),
                action: "pick".into(),
                title: "original commit".into(),
                message: "original commit\n".into(),
            }],
        });
        let mut command = Command::new(
            ["rebase", if running { "--continue" } else { "--start" }],
            Purpose::Action,
        );
        command.service = Some("rebase".into());
        state.write_intent = Some(command);
        state.editor_tasks.insert(41, "write-check".into());
        click(&mut state, "cancel-command").unwrap();
        assert_eq!(
            state.interactive_plan.is_some(),
            running,
            "only an actual ongoing operation may retain its recovery plan"
        );
        assert!(state.write_intent.is_none());
        assert!(state.pending.is_none());
        assert!(state.queue.is_empty());
    }
}

/// Per-row edge limits must not override the workbench's busy state while a read/write is pending.
#[test]
fn rebase_plan_controls_remain_disabled_while_native_work_is_pending() {
    fn find<'a>(node: &'a ui::Node, id: &str) -> Option<&'a ui::Node> {
        if node.id == id {
            return Some(node);
        }
        match &node.kind {
            ui::Kind::Column { children } | ui::Kind::Row { children } => {
                children.iter().find_map(|child| find(child, id))
            }
            ui::Kind::Scroll { content } => find(content, id),
            _ => None,
        }
    }
    let mut state = workbench();
    state.rebase_draft = Some(rebase_model::Plan {
        workspace: "C:/dirty-fixture".into(),
        git: "git".into(),
        base: hash('b'),
        head: hash('a'),
        branch: Some("refs/heads/main".into()),
        original: vec![hash('a'), hash('c')],
        steps: ['a', 'c']
            .map(|character| rebase_model::Step {
                oid: hash(character),
                action: "pick".into(),
                title: "original commit".into(),
                message: "original commit\n".into(),
            })
            .to_vec(),
    });
    state.editor_tasks.insert(41, "write-check".into());
    let dialog = state.rebase_dialog().unwrap();
    for id in [
        "rebase-up-1",
        "rebase-down-0",
        "rebase-action-0",
        "rebase-run",
    ] {
        assert!(
            find(&dialog.content, id).unwrap().disabled,
            "{id} remained enabled while busy"
        );
    }
    assert!(!find(&dialog.content, "rebase-cancel").unwrap().disabled);
}

/// Selecting a complete conflict version creates a new approval, even when a different command
/// was previously blocked by unsaved native text. No part of that revoked approval is inherited.
#[test]
fn new_whole_version_review_replaces_a_revoked_buffer_confirmation() {
    use crate::conflicts::lifecycle::Marker;

    let mut state = workbench();
    state
        .prepare_operation_preview(
            Command::new(["reset", "--hard", "topic"], Purpose::Action),
            "history".into(),
            false,
        )
        .unwrap();
    finish_preview(&mut state, b"");
    state.write_intent = state.pending.take();
    state
        .complete_write_check(&[document(true, 9)])
        .unwrap_err();
    assert!(
        state.pending_recheck,
        "the old native-buffer refusal revoked approval"
    );

    let path = "tracked/after-save.txt";
    let stages = format!(
        "100644 {} 2\t{path}\0100644 {} 3\t{path}\0",
        hash('a'),
        hash('b'),
    )
    .into_bytes();
    let round = state.conflict_lifecycle.begin();
    state.conflict_lifecycle.status(
        round,
        Some(vec![Change {
            path: path.into(),
            conflict: true,
            index_entry: stages.clone(),
            ..Default::default()
        }]),
    );
    let mut publication = None;
    for marker in Marker::ALL {
        let present = marker as usize == Marker::Merge as usize;
        publication = state.conflict_lifecycle.probe(
            round,
            marker,
            if present {
                hash('d').into_bytes()
            } else {
                Vec::new()
            }
            .as_slice(),
            if present { 0 } else { 1 },
        );
    }
    state.publish_conflict_observation(publication).unwrap();
    state.merge_selected = path.into();
    state.merge_files.insert(
        path.into(),
        MergeFile {
            index_entry: stages,
            opaque: true,
            ..Default::default()
        },
    );
    state
        .ui_event(
            Some("conflicts"),
            ui::UiEvent {
                revision: state.revision,
                node: "conflict-stage-2".into(),
                action: ui::Action::Click,
            },
        )
        .unwrap();

    assert_eq!(
        state.pending.as_ref().unwrap().effective_args(),
        ["checkout", "--ours", "--", path]
    );
    assert_eq!(state.pending_panel, "conflicts");
    assert!(!state.pending_dirty);
    assert!(
        !state.pending_recheck,
        "a new whole-version review inherited the old recheck state"
    );
    assert!(state.editor_tasks.is_empty());
    assert!(
        state.queue.is_empty(),
        "new review cannot execute a version choice"
    );
}
