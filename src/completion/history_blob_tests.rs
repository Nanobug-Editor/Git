//! Historical file sides distinguish blob bytes from a replacement directory's tree identity.
use super::*;

/// Completion receives complete process bytes; no native transport or Git child is needed here.
fn running(purpose: Purpose, output: Vec<u8>) -> Running {
    Running {
        handle: api::ResourceHandle {
            instance: "history-kind-test".into(),
            scope: "workspace".into(),
            resource: 1,
        },
        command: Command::new(["ls-tree"], purpose),
        output,
        stderr: Vec::new(),
    }
}

/// The selected deleted file is absent when the same spelling now denotes a directory.
#[test]
fn history_object_parser_distinguishes_blob_tree_and_absent_file_sides() {
    assert_eq!(history_blob_identity(b"").unwrap(), None);
    for width in [40, 64] {
        let oid = "a".repeat(width);
        for mode in ["100644", "100755", "120000"] {
            let record = format!("{mode} blob {oid}\tfoo\0");
            assert_eq!(
                history_blob_identity(record.as_bytes()).unwrap(),
                Some(oid.as_str())
            );
        }
        let directory = format!("040000 tree {oid}\tfoo\0");
        assert_eq!(history_blob_identity(directory.as_bytes()).unwrap(), None);
    }
}

/// A complete empty right side must be published beside the preserved original left file.
/// Neither an absent path nor a tree entry may enqueue `cat-file blob` for that column.
#[test]
fn history_object_tree_completion_builds_an_empty_side_without_a_blob_query() {
    for output in [
        Vec::new(),
        format!("040000 tree {}\tfoo\0", "b".repeat(40)).into_bytes(),
    ] {
        let mut state = State {
            columns: vec!["original file\r\n".into()],
            ..Default::default()
        };
        state
            .finished(running(Purpose::HistoryBlob { column: 1 }, output), 0)
            .unwrap();
        assert!(
            state.queue.is_empty(),
            "a replacement directory is not a readable file blob"
        );
        assert_eq!(state.columns, ["original file\r\n", ""]);
    }
}

/// Real blobs retain their exact text and the existing binary/256-KiB comparison limits.
#[test]
fn history_object_blob_completion_uses_the_existing_bounded_snapshot_handler() {
    let oid = "c".repeat(64);
    let mut state = State {
        columns: vec!["old\n".into()],
        ..Default::default()
    };
    state
        .finished(
            running(
                Purpose::HistoryBlob { column: 1 },
                format!("100644 blob {oid}\tfoo\0").into_bytes(),
            ),
            0,
        )
        .unwrap();
    let query = state.queue.pop_front().unwrap();
    assert_eq!(query.args, ["cat-file", "blob", oid.as_str()]);
    assert!(matches!(query.purpose, Purpose::Snapshot { column: 1 }));
    assert!(state.queue.is_empty());
    state
        .finished(
            running(query.purpose, "新文件😀\r\n".as_bytes().to_vec()),
            0,
        )
        .unwrap();
    assert_eq!(state.columns, ["old\n", "新文件😀\r\n"]);
    for output in [
        vec![b'x'; 256 * 1024 + 1],
        b"binary\0bytes".to_vec(),
        vec![0xff],
    ] {
        assert!(
            state
                .finished(running(Purpose::Snapshot { column: 1 }, output), 0)
                .is_err()
        );
        assert_eq!(
            state.columns[1], "新文件😀\r\n",
            "rejected output must not replace a complete side"
        );
    }
}

/// Unsupported objects and malformed/truncated records cannot masquerade as empty file sides.
#[test]
fn history_object_parser_and_handler_reject_unsupported_or_incomplete_records() {
    let oid = "d".repeat(40);
    let records = [
        format!("160000 commit {oid}\tmodule\0"),
        format!("040000 tag {oid}\tfoo\0"),
        format!("040000 blob {oid}\tfoo\0"),
        format!("100644 tree {oid}\tfoo\0"),
        format!("100644 blob {oid}\tfoo"),
        format!("100644 blob {oid}\0"),
        format!("100644 blob {oid} extra\tfoo\0"),
        "100644 blob \tfoo\0".into(),
        "100644 blob invalid-oid\tfoo\0".into(),
        format!("100644 blob {oid}\tfoo\0{oid}\0"),
        format!("100644 blob {oid}\t\0"),
    ];
    for record in records {
        assert!(
            history_blob_identity(record.as_bytes()).is_err(),
            "unexpected record accepted: {record:?}"
        );
        let mut state = State {
            columns: vec!["preserved\n".into()],
            ..Default::default()
        };
        assert!(
            state
                .finished(
                    running(Purpose::HistoryBlob { column: 1 }, record.into_bytes()),
                    0
                )
                .is_err()
        );
        assert!(state.queue.is_empty());
        assert_eq!(state.columns, ["preserved\n"]);
    }
}
