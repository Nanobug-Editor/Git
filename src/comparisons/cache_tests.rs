//! Immutable historical side reuse is checked through the same queue and completion as real Git.
use super::*;

/// A cold two-sided comparison must cross the process boundary once, not once per lookup and blob.
#[test]
fn cold_history_comparison_batches_both_sides_in_one_process() {
    let mut state = State {
        selected: "commit".into(),
        history_selection: Some(Commit {
            oid: "commit".into(),
            parents: vec!["parent".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    state.history_file("nested/file.rs");
    assert_eq!(state.queue.len(), 1, "each side started its own Git lookup");
    assert!(
        state.queue[0]
            .input
            .as_ref()
            .is_some_and(|input| { input == b"parent:nested/file.rs\0commit:nested/file.rs\0" })
    );
}

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
    let batch = state.queue.pop_front().unwrap();
    let output = format!(
        "parent:file.rs missing\n{} blob 13\nfn main() {{}}\n\n",
        "a".repeat(40)
    );
    finish(&mut state, batch, output.as_bytes()).unwrap();
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
        1,
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
    let invalid = format!("{} blob 8\nnot\0text\n", "a".repeat(40));
    assert!(finish(&mut state, query, invalid.as_bytes()).is_err());
    state.history_file("file.rs");
    assert_eq!(
        state.queue.len(),
        1,
        "failed data cannot satisfy the next immutable read"
    );
}
