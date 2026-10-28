//! Repeated live-conflict and retained-draft paths must share one stable presentation leaf.
use super::*;

/// Opening conflicts adds their paths to the draft cache without removing live Git changes.
/// Their union must preserve first-seen order across the tree's 128-node page boundary, while
/// a resolved file retained only in the draft cache remains reachable at the end.
#[test]
fn conflict_directory_union_keeps_unique_leaves_and_page_boundaries() {
    let live: Vec<_> = (0..130)
        .map(|index| format!("conflicts/{index:03}.txt"))
        .collect();
    let mut drafts: BTreeMap<_, _> = live.iter().cloned().map(|path| (path, ())).collect();
    drafts.insert("conflicts/resolved-only.txt".into(), ());
    let mut directory = Directory::default();
    for path in live.iter().chain(drafts.keys()) {
        directory.insert(path);
    }

    assert!(directory.files.is_empty());
    assert_eq!(
        directory
            .children
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["conflicts"]
    );
    let files = &directory.children["conflicts"].files;
    assert_eq!(
        files.len(),
        131,
        "one original Git path must produce one leaf even after its draft is opened"
    );
    let actual: Vec<_> = files.iter().map(|(_, path)| path.as_str()).collect();
    let expected: Vec<_> = live
        .iter()
        .map(String::as_str)
        .chain(["conflicts/resolved-only.txt"])
        .collect();
    assert_eq!(
        actual, expected,
        "deduplication must retain first-seen order"
    );

    let pages: Vec<_> = files.chunks(128).collect();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].len(), 128);
    assert_eq!(pages[0].last().unwrap().1, "conflicts/127.txt");
    assert_eq!(pages[1].len(), 3);
    assert_eq!(pages[1][0].1, "conflicts/128.txt");
    assert_eq!(pages[1].last().unwrap().1, "conflicts/resolved-only.txt");
}

/// Deduplication is scoped to the complete original path: an equal label at the root or in
/// another directory is a distinct navigable Git file, and repeated inserts keep its first slot.
#[test]
fn directory_unique_paths_keep_root_and_equal_leaf_names_distinct() {
    let mut directory = Directory::default();
    for path in [
        "merge.txt",
        "conflicts/merge.txt",
        "elsewhere/merge.txt",
        "conflicts/merge.txt",
        "merge.txt",
        "elsewhere/merge.txt",
    ] {
        directory.insert(path);
    }

    assert_eq!(directory.files, [("merge.txt".into(), "merge.txt".into())]);
    assert_eq!(
        directory.children["conflicts"].files,
        [("merge.txt".into(), "conflicts/merge.txt".into())]
    );
    assert_eq!(
        directory.children["elsewhere"].files,
        [("merge.txt".into(), "elsewhere/merge.txt".into())]
    );
}
