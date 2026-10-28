//! Branch queries keep the last admitted review visible until a complete replacement is ready.
use super::*;
use crate::model::HistoryChange;

/// A successful rename follows the selected ref; deletion returns to all refs and seals old pages.
#[test]
fn branch_metadata_write_reconciles_selected_history_filter() {
    for (args, replacement) in [
        (
            vec!["branch", "-m", "--", "topic", "topic2"],
            Some("refs/heads/topic2"),
        ),
        (vec!["branch", "-d", "--", "topic"], None),
    ] {
        let mut state = workbench();
        state.filter = Some("refs/heads/topic".into());
        state.queue_log();
        let old_generation = state.history_cache.generation;
        state
            .finished(
                Running {
                    handle: handle(),
                    command: Command::new(args, Purpose::Action),
                    output: Vec::new(),
                    stderr: Vec::new(),
                },
                0,
            )
            .unwrap();
        assert_eq!(state.filter.as_deref(), replacement);
        assert!(state.history_cache.generation > old_generation);
        assert!(state.queue.iter().all(|command| !matches!(&command.purpose, Purpose::Log { filter: Some(reference), .. } if reference == "refs/heads/topic")));
        assert!(state.queue.iter().any(|command| matches!(&command.purpose, Purpose::Log { filter, .. } if filter.as_deref() == replacement)));
    }
}

/// Metadata changes on another ref preserve the user's independent history selection.
#[test]
fn unrelated_branch_rename_preserves_history_filter() {
    let mut state = workbench();
    state.filter = Some("refs/heads/other".into());
    state
        .follow_branch_metadata_change(
            &["branch", "-m", "--", "topic", "topic2"].map(str::to_owned),
        )
        .unwrap();
    assert_eq!(state.filter.as_deref(), Some("refs/heads/other"));
}

/// Returning to a visited branch paints its own first page before any new Git receipt arrives.
#[test]
fn branch_switch_restores_a_visited_log_without_waiting_for_git() {
    let repository = commits();
    let mut state = workbench();
    state.refs = vec!["refs/heads/main".into(), "refs/heads/topic".into()];
    state.trees.insert("branches".into(), handle());
    for branch in ["main", "topic"] {
        state.tree_paths.insert(
            format!("branches:{branch}"),
            trees::TreePath::Leaf(format!("local/{branch}")),
        );
    }
    let select = |state: &mut State, branch: &str| {
        state
            .tree_event(tree::Event::Selected {
                tree: handle(),
                epoch: 1,
                node: branch.into(),
            })
            .unwrap();
    };
    select(&mut state, "main");
    complete_history(&mut state, &repository[..10]);
    select(&mut state, "topic");
    complete_history(&mut state, &repository[10..20]);
    let started = std::time::Instant::now();
    select(&mut state, "main");
    assert_eq!(
        state.commits[0].oid, repository[0].oid,
        "visited branch must paint immediately, without waiting for another Git process"
    );
    assert!(
        !state
            .queue
            .iter()
            .any(|command| matches!(command.purpose, Purpose::Log { .. })),
        "an unchanged visited branch must reuse its admitted log page"
    );
    eprintln!("visited branch selection: {:?}", started.elapsed());
}

/// Drive the real branch-leaf dispatcher without completing its new asynchronous log request.
#[test]
fn branch_filter_keeps_loaded_review_until_replacement_is_ready() {
    let repository = commits();
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &repository);
    state.select_history_row(5, false);
    state.queue.clear();
    state.history_files = vec!["src/selected.rs".into()];
    state.history_message = "Existing complete commit message\n".into();
    state.changes = vec![Change {
        path: "src/draft.rs".into(),
        worktree: 'M',
        ..Default::default()
    }];
    state.refs = vec!["refs/heads/topic".into()];
    state.trees.insert("branches".into(), handle());
    state.tree_paths.insert(
        "branches:topic".into(),
        trees::TreePath::Leaf("local/topic".into()),
    );
    let before = state
        .commits
        .iter()
        .map(|commit| commit.oid.clone())
        .collect::<Vec<_>>();
    assert!(
        state
            .tree_event(tree::Event::Selected {
                tree: handle(),
                epoch: 1,
                node: "topic".into()
            })
            .unwrap()
    );
    assert_eq!(state.filter.as_deref(), Some("refs/heads/topic"));
    assert_eq!(
        state
            .commits
            .iter()
            .map(|commit| commit.oid.clone())
            .collect::<Vec<_>>(),
        before,
        "branch selection must retain the visible log until new data is ready, rather than show a blank canvas"
    );
    assert_eq!(state.history_files, ["src/selected.rs"]);
    assert_eq!(state.history_message, "Existing complete commit message\n");
    assert_eq!(state.changes[0].path, "src/draft.rs");
    complete_history(&mut state, &repository[..2]);
    assert_eq!(
        state.commits.len(),
        2,
        "completed new branch replaces the old review"
    );
}

/// Repeatedly selecting an already active filter must not discard or re-query its review.
#[test]
fn selecting_current_branch_filter_keeps_review_and_avoids_requery() {
    let repository = commits();
    let mut state = workbench();
    state.queue_log();
    complete_history(&mut state, &repository);
    state.filter = Some("refs/heads/topic".into());
    state.refs = vec!["refs/heads/topic".into()];
    state.trees.insert("branches".into(), handle());
    state.tree_paths.insert(
        "branches:topic".into(),
        trees::TreePath::Leaf("local/topic".into()),
    );
    state.queue.clear();
    state
        .tree_event(tree::Event::Selected {
            tree: handle(),
            epoch: 1,
            node: "topic".into(),
        })
        .unwrap();
    assert_eq!(
        state.commits.len(),
        128,
        "same filter cannot clear the loaded log"
    );
    assert!(
        state.queue.is_empty(),
        "same filter cannot trigger another log query"
    );
}

/// A selected commit cannot replace a complete preview with empty files or mismatched metadata.
#[test]
fn commit_preview_keeps_files_and_message_until_both_new_receipts_are_ready() {
    let repository = commits();
    let mut state = workbench();
    state.commits = repository[..2].to_vec();
    state.selected = repository[0].oid.clone();
    state.history_selection = Some(repository[0].clone());
    state.history_preview_commit = Some(repository[0].clone());
    state.history_files = vec!["old.txt".into()];
    state.history_message = "old complete message".into();
    state.trees.insert("files".into(), handle());
    state.select_history_row(1, false);
    let before = state.history_preview();
    assert!(
        find_node(&before, "files").is_some(),
        "loading cannot replace the admitted native file tree"
    );
    assert!(find_node(&before, "history-files-loading").is_none());
    let command = state.queue.pop_front().unwrap();
    state
        .finished(
            Running {
                handle: handle(),
                command,
                output: b"A\0new.txt\0".to_vec(),
                stderr: Vec::new(),
            },
            0,
        )
        .unwrap();
    assert_eq!(state.history_files, ["old.txt"]);
    assert_eq!(state.history_message, "old complete message");
    let command = state.queue.pop_front().unwrap();
    state
        .finished(
            Running {
                handle: handle(),
                command,
                output: b"new complete message".to_vec(),
                stderr: Vec::new(),
            },
            0,
        )
        .unwrap();
    assert_eq!(state.history_files, ["new.txt"]);
    assert_eq!(state.history_message, "new complete message");
    assert_eq!(state.history_preview_commit.unwrap().oid, repository[1].oid);
}

/// A rejected file delta preserves the complete old preview rather than mixing two commit receipts.
#[test]
fn rejected_preview_patch_keeps_last_complete_files_and_message() {
    let repository = commits();
    let mut state = workbench();
    state.commits = repository[..2].to_vec();
    state.selected = repository[1].oid.clone();
    state.history_selection = Some(repository[1].clone());
    state.history_preview_commit = Some(repository[0].clone());
    state.history_files = vec!["old.txt".into()];
    state.history_file_status = BTreeMap::from([("old.txt".into(), HistoryChange::Added)]);
    state.history_message = "old complete message".into();
    state.trees.insert("files".into(), handle());
    let total = format!("d{}", crate::trees::node_id(""));
    state.tree_paths.insert(
        format!("files:{total}"),
        trees::TreePath::Directory(String::new()),
    );
    state.tree_pages.insert(
        "files".into(),
        BTreeMap::from([
            (None, Default::default()),
            (Some(total), Default::default()),
        ]),
    );
    // The public metadata budget rejects this provider filename before any SDK transport call.
    state.pending_history_files = Some(BTreeMap::from([("x".repeat(1025), HistoryChange::Added)]));
    let error = state
        .finished(
            Running {
                handle: handle(),
                command: Command::new(["show"], Purpose::CommitMessage(repository[1].oid.clone())),
                output: b"new complete message".to_vec(),
                stderr: Vec::new(),
            },
            0,
        )
        .unwrap_err();
    assert_eq!(error.code, api::ErrorCode::LimitExceeded);
    assert_eq!(state.history_files, ["old.txt"]);
    assert_eq!(state.history_file_status["old.txt"], HistoryChange::Added);
    assert_eq!(state.history_message, "old complete message");
    assert_eq!(state.history_preview_commit.unwrap().oid, repository[0].oid);
}
