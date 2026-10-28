//! Batch receipts preserve operation identity and reject partial or mismatched observations.
use super::*;

/// Missing replies name each request, so a different checkout or unresolved object is distinguishable.
fn absent(prefix: &str) -> String {
    Marker::ALL
        .iter()
        .map(|marker| format!("{prefix}{} missing\n", marker.reference()))
        .collect()
}

#[test]
fn operation_batch_waits_for_status_and_ignores_retired_rounds() {
    let mut lifecycle = Lifecycle::default();
    let old = lifecycle.begin();
    let current = lifecycle.begin();
    assert!(
        lifecycle
            .probes(old, "", absent("").as_bytes(), 0)
            .is_none()
    );
    assert!(!lifecycle.ready());
    let prefix = "worktrees/linked/";
    let bytes = absent(prefix).replacen(&format!("{prefix}MERGE_HEAD missing"), &"a".repeat(40), 1);
    assert!(
        lifecycle
            .probes(current, prefix, bytes.as_bytes(), 0)
            .is_none()
    );
    assert!(!lifecycle.ready());
    let published = lifecycle.status(current, Some(vec![])).unwrap().unwrap();
    assert_eq!(published.operation.as_deref(), Some("merge"));
    assert_eq!(published.oid, Some("a".repeat(40)));
}

#[test]
fn operation_batch_failure_never_reopens_stale_proposals() {
    let normal = absent("");
    for (bytes, code) in [
        (normal.as_bytes().to_vec(), 128),
        (
            normal
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes(),
            0,
        ),
        (format!("{normal}extra\n").into_bytes(), 0),
        (absent("worktrees/wrong/").into_bytes(), 0),
        (
            normal
                .replacen(
                    "MERGE_HEAD missing",
                    &format!("{} missing", "a".repeat(40)),
                    1,
                )
                .into_bytes(),
            0,
        ),
        (vec![255], 0),
    ] {
        let mut lifecycle = Lifecycle::default();
        let first = lifecycle.begin();
        lifecycle.probes(first, "", normal.as_bytes(), 0);
        assert!(lifecycle.status(first, Some(vec![])).unwrap().is_ok());
        let next = lifecycle.begin();
        lifecycle.probes(next, "", &bytes, code);
        assert!(lifecycle.pending_failed());
        assert!(!lifecycle.ready());
        assert!(lifecycle.status(next, Some(vec![])).unwrap().is_err());
        lifecycle.cancel_observation();
        assert!(!lifecycle.ready());
    }
}
