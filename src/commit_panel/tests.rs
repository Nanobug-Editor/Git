//! Commit checkboxes represent a local file range, independent of the real index and lazy pages.
use super::*;

/// Unversioned checks select descendants locally without inspecting buffers or writing Git's index.
#[test]
fn commit_untracked_checkbox_only_selects_and_keeps_its_group() {
    let handle = api::ResourceHandle {
        instance: "commit-untracked-test".into(),
        scope: "workspace".into(),
        resource: 1,
    };
    let mut state = State::default();
    state.changes = vec![
        change("new/a.txt", '.', '?'),
        change("new/deep/b.txt", '.', '?'),
        change("new-other/c.txt", '.', '?'),
    ];
    state.trees.insert("staging".into(), handle.clone());
    state.tree_paths.insert(
        "staging:directory".into(),
        TreePath::Directory("untracked/new".into()),
    );
    assert!(
        state
            .tree_event(tree::Event::Checked {
                tree: handle,
                epoch: 1,
                node: "directory".into(),
                previous: tree::CheckState::Unchecked,
                checked: true,
            })
            .unwrap()
    );
    assert!(
        state.commit_stage_intent.is_none(),
        "selection must not inspect dirty buffers or stage files"
    );
    assert!(
        state.queue.is_empty(),
        "selection must not start Git processes"
    );
    for (path, checked) in [
        ("untracked/new", tree::CheckState::Checked),
        ("untracked/new/a.txt", tree::CheckState::Checked),
        ("untracked/new/deep/b.txt", tree::CheckState::Checked),
        ("untracked/new-other/c.txt", tree::CheckState::Unchecked),
    ] {
        let path = if path == "untracked/new" {
            TreePath::Directory(path.into())
        } else {
            TreePath::Leaf(path.into())
        };
        assert_eq!(
            state.provider_node("staging", "test", &path).checked,
            Some(checked)
        );
    }
    assert!(
        state
            .commit_changes(&TreePath::Directory("changes".into()))
            .is_empty()
    );
    assert_eq!(
        state
            .commit_changes(&TreePath::Directory("untracked".into()))
            .len(),
        3
    );
}

/// A checkbox updates local inclusion without withdrawing the tree or scheduling an index write.
#[test]
fn commit_checkbox_keeps_tree_visible_and_admitted_during_index_write() {
    let handle = api::ResourceHandle {
        instance: "commit-checkbox-test".into(),
        scope: "workspace".into(),
        resource: 1,
    };
    let mut state = State::default();
    state.commit_message = "保留提交草稿".into();
    state.changes = vec![change("src/staged.rs", 'A', '.')];
    state.commit_selected.insert("src/staged.rs".into());
    state.trees.insert("staging".into(), handle.clone());
    state.tree_paths.insert(
        "staging:directory".into(),
        TreePath::Directory("changes/src".into()),
    );
    let before = ui::Document::new(state.commit_panel());
    assert!(before.active_node("staging").is_some());
    assert!(
        state
            .tree_event(tree::Event::Checked {
                tree: handle,
                epoch: 1,
                node: "directory".into(),
                previous: tree::CheckState::Checked,
                checked: false,
            })
            .unwrap()
    );
    assert!(
        state.queue.is_empty(),
        "a local exclusion cannot start an index write"
    );
    assert!(!state.commit_check("directory", false).unwrap());
    assert_eq!(
        state.queue.len(),
        0,
        "repeated local choices cannot enqueue writes"
    );
    let during = ui::Document::new(state.commit_panel());
    assert!(
        during.active_node("staging").is_some(),
        "checkbox write withdrew the whole commit tree while its index guard was pending"
    );
    assert_eq!(
        during.active_node("commit-message"),
        before.active_node("commit-message")
    );
}

/// Operation diagnostics no longer appear in or resize the stationary commit form.
#[test]
fn commit_checkbox_feedback_keeps_composer_geometry() {
    let mut state = State::default();
    let before = ui::Document::new(state.commit_panel());
    let mut ids = Vec::new();
    before.root.visit(&mut |node| ids.push(node.id.clone()));
    for removed in ["commit-summary", "commit-feedback", "status"] {
        assert!(!ids.iter().any(|id| id == removed));
    }
    assert_eq!(
        before.active_node("commit-message").unwrap().textarea_rows,
        Some(ui::TextareaRows { min: 2, max: 4 })
    );
    for message in [
        "Git 操作完成 / Operation completed",
        "index.lock: File exists",
    ] {
        state.message = message.into();
        let after = ui::Document::new(state.commit_panel());
        let geometry = |document: &ui::Document| {
            let ui::Kind::Column { children } =
                &document.active_node("commit-composer").unwrap().kind
            else {
                panic!("composer must stack its fixed controls");
            };
            children
                .iter()
                .map(|node| (node.id.clone(), node.layout.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            geometry(&after),
            geometry(&before),
            "feedback resized the composer"
        );
    }
}

/// Removing inline feedback also removes its old button, event and details dialog.
#[test]
fn commit_lock_error_keeps_the_composer_compact() {
    let mut state = State::default();
    state.message = "fatal: Unable to create 'repo/.git/index.lock': File exists.\n\nAnother git process seems to be running in this repository.\n".repeat(5);
    state.commit_message = "保留提交草稿".into();
    let document = ui::Document::new(state.commit_panel());
    document.validate().unwrap();
    document.root.visit(&mut |node| {
        if let ui::Kind::Text { text } = &node.kind {
            assert!(
                !text.contains("index.lock"),
                "raw stderr expanded the composer"
            );
        }
    });
    assert!(document.active_node("status").is_none());
    assert!(
        !state
            .commit_panel_event(&ui::UiEvent {
                revision: 0,
                node: "status".into(),
                action: ui::Action::Click,
            })
            .unwrap()
    );
    assert_eq!(state.commit_message, "保留提交草稿");
}

/// Collapsed directories include every descendant locally while similarly prefixed siblings stay out.
#[test]
fn commit_directory_range_covers_all_descendants_without_index_writes() {
    let mut state = State::default();
    state.changes = (0..471)
        .map(|index| change(&format!("crates/editor-app/src/deep/{index}.rs"), '.', 'M'))
        .collect();
    state
        .changes
        .push(change("crates/editor-app-other/file.rs", '.', 'M'));
    let path = TreePath::Directory("changes/crates/editor-app".into());
    state
        .tree_paths
        .insert("staging:directory".into(), path.clone());
    for checked in [true, false] {
        assert!(state.commit_check("directory", checked).unwrap());
        assert_eq!(state.commit_selected.len(), if checked { 471 } else { 0 });
        assert!(state.queue.is_empty());
        assert!(state.editor_tasks.is_empty());
        assert!(
            state
                .changes
                .iter()
                .all(|change| change.index == '.' && change.worktree == 'M')
        );
        assert_eq!(
            state.commit_selection_state(&state.commit_changes(&path)),
            Some(if checked {
                tree::CheckState::Checked
            } else {
                tree::CheckState::Unchecked
            })
        );
    }
}

/// Checkbox-only patches preserve lazy pages and distinguish a deleted file from its replacement directory.
#[test]
fn commit_range_patch_updates_only_inclusion_and_counts_unloaded_descendants() {
    let mut state = State::default();
    state.changes = vec![
        change("foo", 'D', '.'),
        change("foo/bar", 'A', '.'),
        change("foo/baz", '.', 'M'),
        change("other", '.', 'M'),
    ];
    let entries = [
        ("group", TreePath::Directory("changes".into())),
        ("folder", TreePath::Directory("changes/foo".into())),
        ("old-file", TreePath::Leaf("changes/foo".into())),
        ("leaf", TreePath::Leaf("changes/foo/bar".into())),
        ("other", TreePath::Leaf("changes/other".into())),
    ];
    let nodes = entries
        .iter()
        .map(|(id, path)| state.provider_node("staging", id, path))
        .collect::<Vec<_>>();
    for (id, path) in entries {
        state.tree_paths.insert(format!("staging:{id}"), path);
    }
    let page = |nodes, next| trees::incremental::CachedBranch { nodes, next };
    state.tree_pages.insert(
        "staging".into(),
        BTreeMap::from([
            (None, page(vec![nodes[0].clone()], None)),
            (
                Some("group".into()),
                page(
                    vec![nodes[1].clone(), nodes[2].clone(), nodes[4].clone()],
                    None,
                ),
            ),
            (
                Some("folder".into()),
                page(vec![nodes[3].clone()], Some("1".into())),
            ),
        ]),
    );
    state.commit_selected.insert("foo/bar".into());
    let patch = state.commit_selection_patch();
    assert!(
        patch.branches.is_empty(),
        "selection replaced a cached page or cursor"
    );
    assert_eq!(patch.nodes.len(), 3);
    for node in &patch.nodes {
        let mut original = nodes
            .iter()
            .find(|before| before.id == node.id)
            .unwrap()
            .clone();
        original.checked = node.checked;
        assert_eq!(&original, node, "selection changed non-checkbox metadata");
        assert_eq!(
            node.checked,
            Some(if node.id == "leaf" {
                tree::CheckState::Checked
            } else {
                tree::CheckState::Mixed
            })
        );
        assert_ne!(node.id, "old-file");
    }
    assert_eq!(
        state.tree_pages["staging"][&Some("folder".into())]
            .next
            .as_deref(),
        Some("1")
    );
}

/// Refs may reach the visible branch tree before HEAD; later staging must not retarget cached menus.
#[test]
fn branch_comparison_intent_survives_the_initial_head_read() {
    let mut state = State::default();
    state.refs = vec!["refs/heads/main".into(), "refs/heads/feature".into()];
    state.current_ref = state.refs[0].clone();
    let commands = |items: Vec<ui::ActionMenuItem>| {
        let mut result = Vec::new();
        for item in items {
            item.visit(&mut |item| {
                if let ui::ActionMenuKind::Action { command } = &item.kind {
                    result.push((
                        item.id.clone(),
                        command.command.clone(),
                        command.arguments.clone(),
                    ));
                }
            });
        }
        result
    };
    let pending = commands(state.branch_actions("refs/heads/feature"));
    state.head = Some("a".repeat(40));
    let ready = commands(state.branch_actions("refs/heads/feature"));
    assert_eq!(pending, ready);
    assert!(
        ready
            .iter()
            .any(|(_, _, arguments)| arguments["action"] == "compare-current")
    );
}

/// Minimal status observation without I/O; each file can have independent index/worktree columns.
fn change(path: &str, index: char, worktree: char) -> Change {
    Change {
        path: path.into(),
        index,
        worktree,
        ..Default::default()
    }
}

/// Partial files occur once, group counts include lazy descendants, and directory names are not colored.
#[test]
fn commit_tree_groups_and_checks_preserve_partial_index() {
    let mut state = State::default();
    state.changes = vec![
        change("src/partial.rs", 'M', 'M'),
        change("src/staged.rs", 'A', '.'),
        change("src/unstaged.rs", '.', 'M'),
        change("new.txt", '.', '?'),
    ];
    // Local inclusion can select the whole file even when Git independently has a partial index entry.
    state
        .commit_selected
        .extend(["src/partial.rs".into(), "src/staged.rs".into()]);
    let group = state.provider_node("staging", "group", &TreePath::Directory("changes".into()));
    assert_eq!(group.checked, Some(tree::CheckState::Mixed));
    assert_eq!(group.suffix, "3 个文件");
    assert!(group.tone.is_none());
    for (path, expected) in [
        ("changes/src/partial.rs", tree::CheckState::Checked),
        ("changes/src/staged.rs", tree::CheckState::Checked),
        ("changes/src/unstaged.rs", tree::CheckState::Unchecked),
        ("untracked/new.txt", tree::CheckState::Unchecked),
    ] {
        assert_eq!(
            state
                .provider_node("staging", "file", &TreePath::Leaf(path.into()))
                .checked,
            Some(expected)
        );
    }
    state.commit_message = "selected files".into();
    let (_, guard) = state.commit_submission_command(false).unwrap();
    let Purpose::CommitSelection { expected, next, .. } = guard.purpose else {
        panic!("missing submission guard")
    };
    assert_eq!(expected.len(), 2);
    assert_eq!(next.input.unwrap(), b"src/partial.rs\0src/staged.rs\0");
    assert_eq!(
        (state.changes[0].index, state.changes[0].worktree),
        ('M', 'M')
    );
}

/// One selected rename includes its source deletion and literal destination in the same commit range.
#[test]
fn commit_range_includes_both_names_of_a_rename() {
    let mut state = State::default();
    let mut renamed = change("src/new name.txt", 'R', '.');
    renamed.original = Some("src/old [name].txt".into());
    state.changes = vec![renamed];
    state.commit_selected.insert("src/new name.txt".into());
    state.commit_message = "rename only".into();
    let (paths, command) = state.commit_submission_command(false).unwrap();
    assert_eq!(paths, ["src/new name.txt", "src/old [name].txt"]);
    let Purpose::CommitSelection { next, .. } = command.purpose else {
        panic!("missing submission guard")
    };
    assert_eq!(
        next.input.unwrap(),
        b"src/new name.txt\0src/old [name].txt\0"
    );
    assert_eq!(next.args[0], "commit");
    assert!(next.args.contains(&"--only".into()));
}

/// Conflicts have no checkbox; pathname boundaries exclude similarly prefixed siblings.
#[test]
fn commit_group_selection_is_bounded_to_exact_directory() {
    let mut state = State::default();
    state.changes = vec![change("src/a", '.', 'M'), change("src2/b", '.', 'M')];
    state.changes.push(Change {
        path: "src/conflict".into(),
        conflict: true,
        ..Default::default()
    });
    assert_eq!(
        state
            .commit_changes(&TreePath::Directory("changes/src".into()))
            .len(),
        1
    );
    assert!(
        state
            .provider_node(
                "staging",
                "conflict",
                &TreePath::Directory("conflicts".into())
            )
            .checked
            .is_none()
    );
    state.commit_message = "draft\n第二行".into();
    let document = ui::Document::new(state.commit_panel());
    document.validate().unwrap();
    let mut disabled = None;
    document.root.visit(&mut |node| {
        if node.id == "commit" {
            disabled = Some(node.disabled);
        }
    });
    assert_eq!(disabled, Some(true));
    assert!(
        matches!(&document.active_node("commit-message").unwrap().kind, ui::Kind::Textarea(input) if input.value == state.commit_message)
    );
}
