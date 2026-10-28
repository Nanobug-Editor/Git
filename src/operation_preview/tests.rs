//! Synthetic process receipts exercise the same asynchronous correlation without launching Git.
use super::*;

fn hash(character: char) -> String {
    character.to_string().repeat(40)
}

/// Extract only the existing public process purpose; no native request or lifetime is fabricated.
fn query(state: &mut State) -> (Vec<String>, Query) {
    let command = state.queue.pop_front().expect("next preview query");
    let Purpose::OperationPreview(query) = command.purpose else {
        panic!("preview query")
    };
    (command.args, query)
}

fn finish(state: &mut State, bytes: &[u8]) {
    let (_, query) = query(state);
    state
        .operation_preview_finished(query, bytes, b"", 0)
        .unwrap();
}

fn prepared() -> State {
    let mut state = State::default();
    assert!(
        state
            .prepare_operation_preview(
                Command::new(["reset", "--hard", "topic"], Purpose::Action),
                "history".into(),
                true,
            )
            .unwrap()
    );
    state
}

/// Drive the bounded metadata queries up to the final independent HEAD observation.
fn through_history(state: &mut State) {
    finish(state, b"refs/heads/main\n");
    finish(state, format!("{}\n", hash('a')).as_bytes());
    let (args, target) = query(state);
    assert_eq!(args.last().unwrap(), "topic^{commit}");
    state
        .operation_preview_finished(target, format!("{}\n", hash('b')).as_bytes(), b"", 0)
        .unwrap();
    finish(
        state,
        format!("{}\0\0original commit\0", hash('a')).as_bytes(),
    );
    finish(
        state,
        format!("refs/remotes/origin/main\t{}\n", hash('a')).as_bytes(),
    );
    finish(state, b""); // Empty target tree fixture.
    finish(state, b""); // Complete untracked inventory, including ignored paths.
    finish(state, b"refs/heads/main\n");
}

#[test]
fn selection_changes_cannot_replace_a_preparing_or_prepared_target() {
    let mut state = prepared();
    state.argument = "different-ui-ref".into();
    assert!(
        state
            .prepare_operation_preview(
                Command::new(["rebase", "other"], Purpose::Action),
                "staging".into(),
                false
            )
            .is_err()
    );
    through_history(&mut state);
    finish(&mut state, format!("{}\n", hash('a')).as_bytes());
    let pending = state.pending.as_ref().unwrap();
    assert_eq!(
        pending.effective_args(),
        [
            "reset".into(),
            "--hard".into(),
            "--no-recurse-submodules".into(),
            hash('b')
        ]
    );
    assert!(pending.preview().contains("origin/main"));
    assert!(!pending.preview().contains("different-ui-ref"));
    assert_eq!(state.pending_panel, "history");
    assert!(state.pending_dirty);
    assert!(state.operation_preview.draft.is_none());
}

#[test]
fn late_results_after_cancel_cannot_reopen_confirmation() {
    let mut state = prepared();
    let (_, cancelled) = query(&mut state);
    state.cancel_operation_preview();
    state
        .operation_preview_finished(cancelled, b"refs/heads/main\n", b"", 0)
        .unwrap();
    assert!(state.pending.is_none());
    assert!(state.queue.is_empty());
    assert!(state.operation_preview.draft.is_none());
}

#[test]
fn changed_head_and_failed_git_query_never_publish_an_approval() {
    let mut state = prepared();
    through_history(&mut state);
    let (_, last) = query(&mut state);
    let error = state
        .operation_preview_finished(last, format!("{}\n", hash('c')).as_bytes(), b"", 0)
        .unwrap_err();
    assert!(error.to_string().contains("HEAD changed"));
    assert!(state.pending.is_none());
    assert!(state.operation_preview.draft.is_none());
    let mut state = prepared();
    let (_, first) = query(&mut state);
    let error = state
        .operation_preview_finished(first, b"", b"fatal: ambiguous HEAD", 128)
        .unwrap_err();
    assert!(error.to_string().contains("fatal: ambiguous HEAD"));
    assert!(error.to_string().contains("128"));
    assert!(state.pending.is_none());
}
