//! Immutable historical side reuse is checked through the same queue and completion as real Git.
use super::*;

/// Finish one selected historical side with literal Git output, without native source allocation.
fn finish(state: &mut State, command: Command, output: &[u8]) -> Result<(), api::Failure> {
    state.finished(
        Running {
            handle: api::ResourceHandle {
                instance: "history-cache-test".into(),
                scope: "workspace".into(),
                resource: 1,
            },
            command,
            output: output.to_vec(),
            stderr: Vec::new(),
        },
        0,
    )
}

/// Successful blob and absent-side observations remain immutable across back/forward navigation.
#[test]
fn historical_file_roundtrip_reuses_complete_blob_and_absent_sides() {
    let mut state = State {
        selected: "commit".into(),
        history_selection: Some(Commit {
            oid: "commit".into(),
            parents: vec!["parent".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    state.environment.workspace = "repo-a".into();
    state.history_file("file.rs");
    let left = state.queue.pop_front().unwrap();
    finish(&mut state, left, b"").unwrap();
    let right = state.queue.pop_front().unwrap();
    let tree = format!("100644 blob {}\tfile.rs\0", "a".repeat(40));
    finish(&mut state, right, tree.as_bytes()).unwrap();
    let blob = state.queue.pop_front().unwrap();
    finish(&mut state, blob, b"fn main() {}\n").unwrap();
    state.history_file("file.rs");
    assert!(
        state.queue.is_empty(),
        "back/forward reread immutable Git sources"
    );
    assert_eq!(state.columns, ["", "fn main() {}\n"]);
    state.environment.workspace = "repo-b".into();
    state.history_file("file.rs");
    assert_eq!(
        state.queue.len(),
        2,
        "a source cache must belong to its repository"
    );
}

/// Both entry count and retained text size evict older sides instead of retaining a repository's history.
#[test]
fn immutable_history_cache_is_bounded_by_entries_and_bytes() {
    let mut cache = HistorySources::default();
    for index in 0..65 {
        cache.remember("repo", "oid", &format!("{index}.rs"), Some(""));
    }
    assert!(cache.get("repo", "oid", "0.rs").is_none());
    assert_eq!(cache.get("repo", "oid", "64.rs"), Some(Some(String::new())));
    cache.remember("repo", "oid", "absent", None);
    assert_eq!(cache.get("repo", "oid", "absent"), Some(None));
    let text = "x".repeat(256 * 1024);
    for index in 0..40 {
        cache.remember("repo", "large-oid", &format!("{index}.rs"), Some(&text));
    }
    assert!(cache.get("repo", "large-oid", "0.rs").is_none());
    assert_eq!(
        cache
            .get("repo", "large-oid", "39.rs")
            .unwrap()
            .unwrap()
            .len(),
        text.len()
    );
    assert!(cache.get("repo", "different-oid", "39.rs").is_none());
}

/// A failed blob cannot become a cached empty side on a later visit to the same file.
#[test]
fn rejected_historical_blob_requires_a_fresh_query() {
    let mut state = State {
        selected: "commit".into(),
        history_selection: Some(Commit {
            oid: "commit".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    state.history_file("file.rs");
    let query = state.queue.pop_front().unwrap();
    finish(
        &mut state,
        query,
        format!("100644 blob {}\tfile.rs\0", "a".repeat(40)).as_bytes(),
    )
    .unwrap();
    let blob = state.queue.pop_front().unwrap();
    assert!(finish(&mut state, blob, b"not\0text").is_err());
    state.history_file("file.rs");
    assert_eq!(
        state.queue.len(),
        1,
        "failed data cannot satisfy the next immutable read"
    );
}
