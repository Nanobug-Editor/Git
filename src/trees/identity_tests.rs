//! A deleted Git file and its replacement directory share a spelling but remain distinct nodes.
use super::*;

/// Both status and diff-tree legally retain `foo` beside `foo/bar` during a directory replacement.
fn replacement(tree_name: &str) -> State {
    let mut state = State {
        revision: 7,
        selected_path: "previous.txt".into(),
        history_path: "previous.txt".into(),
        changes: vec![
            Change {
                path: "foo".into(),
                index: 'D',
                worktree: '.',
                ..Default::default()
            },
            Change {
                path: "foo/bar".into(),
                index: 'A',
                worktree: '.',
                ..Default::default()
            },
        ],
        history_files: vec!["foo".into(), "foo/bar".into()],
        // A history leaf now opens a captured readonly editor comparison, so it needs a
        // selected immutable commit just as the real diff-tree publication does.
        selected: "a".repeat(40),
        history_selection: Some(Commit {
            oid: "a".repeat(40),
            parents: vec!["b".repeat(40)],
            ..Default::default()
        }),
        ..Default::default()
    };
    state.trees.insert(tree_name.into(), handle());
    state
}

fn handle() -> api::ResourceHandle {
    api::ResourceHandle {
        instance: "tree-kind-test".into(),
        scope: "workspace".into(),
        resource: 1,
    }
}

/// Directories and observed refs keep different artwork at every local/remote nesting level.
#[test]
fn branch_tree_artwork_distinguishes_directories_from_local_and_remote_refs() {
    let mut state = State {
        refs: vec![
            "refs/heads/main".into(),
            "refs/heads/feature/topic".into(),
            "refs/remotes/origin/main".into(),
        ],
        ..Default::default()
    };
    state.trees.insert("branches".into(), handle());
    let mut pending = std::collections::VecDeque::from([None]);
    let mut branches = 0;
    let mut directories = 0;
    while let Some(parent) = pending.pop_front() {
        for node in page(&mut state, parent).unwrap().nodes {
            if node.branch {
                directories += 1;
                assert_eq!(
                    node.icon,
                    Some(ui::ToolIcon::new(
                        "icons/folder.svg",
                        "icons/folder-dark.svg"
                    ))
                );
                pending.push_back(Some(node.id));
            } else {
                branches += 1;
                assert_eq!(
                    node.icon,
                    Some(ui::ToolIcon::new("icons/git.svg", "icons/git.svg"))
                );
            }
        }
    }
    assert_eq!(directories, 4);
    assert_eq!(branches, 3);
}

/// One visible lazy page must not register every unrequested sibling for later menu updates.
#[test]
fn branch_metadata_updates_track_only_published_lazy_pages() {
    let mut state = State {
        refs: (0..200)
            .map(|i| format!("refs/heads/feature/topic-{i:03}"))
            .collect(),
        ..Default::default()
    };
    state.trees.insert("branches".into(), handle());
    let mut parent = None;
    for expected in 1..=3 {
        let page = state
            .tree_page(&tree::Request {
                tree: handle(),
                epoch: 1,
                sequence: expected,
                parent,
                cursor: None,
                max_nodes: 1,
            })
            .unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(state.tree_paths.len(), expected as usize);
        let node = &page.nodes[0];
        if expected < 3 {
            assert!(node.branch);
        } else {
            assert!(!node.branch);
            assert_eq!(node.label, "topic-000");
            assert!(page.next.is_some());
        }
        parent = Some(node.id.clone());
    }
}

/// Exercise the production page builder without replacing the SDK's native transport.
fn page(state: &mut State, parent: Option<String>) -> Result<tree::Page, api::Failure> {
    state.tree_page(&tree::Request {
        tree: handle(),
        epoch: 1,
        sequence: 1,
        parent,
        cursor: None,
        max_nodes: 128,
    })
}

/// The checked-out leaf leads its own first lazy page, including before child directories;
/// a nested current branch never escapes its parent or marks its remote namesake as current.
#[test]
fn branch_current_leaf_leads_its_own_level_before_pagination() {
    for (reference, parent, label) in [
        ("refs/heads/main", "local", "main"),
        ("refs/heads/codex/z-topic", "local/codex", "z-topic"),
    ] {
        let mut state = State {
            refs: vec![
                "refs/heads/main".into(),
                "refs/heads/a-first".into(),
                "refs/heads/codex/a-first".into(),
                "refs/heads/codex/z-topic".into(),
                "refs/heads/codex/nested/topic".into(),
                "refs/remotes/origin/main".into(),
            ],
            current_ref: reference.into(),
            // The selected history is deliberately different from the checked-out branch.
            filter: Some("refs/heads/a-first".into()),
            ..Default::default()
        };
        state.trees.insert("branches".into(), handle());
        let mut pending = std::collections::VecDeque::from([None]);
        while let Some(parent) = pending.pop_front() {
            for node in page(&mut state, parent).unwrap().nodes {
                if node.branch {
                    pending.push_back(Some(node.id));
                }
            }
        }
        let first = state
            .tree_page(&tree::Request {
                tree: handle(),
                epoch: 1,
                sequence: 2,
                parent: Some(format!("d{}", node_id(parent))),
                cursor: None,
                max_nodes: 1,
            })
            .unwrap();
        assert_eq!(first.nodes[0].label, label);
        assert!(!first.nodes[0].branch);
        assert_eq!(
            first.nodes[0].icon,
            Some(ui::ToolIcon::new(
                "icons/current-branch.svg",
                "icons/current-branch-dark.svg"
            ))
        );
        assert!(first.next.is_some());
        let remote = state.provider_node(
            "branches",
            "remote-main",
            &TreePath::Leaf("remote/origin/main".into()),
        );
        assert_eq!(
            remote.icon,
            Some(ui::ToolIcon::new("icons/git.svg", "icons/git.svg"))
        );
        if parent != "local" {
            assert_eq!(
                page(&mut state, Some(format!("d{}", node_id("local"))))
                    .unwrap()
                    .nodes[0]
                    .label,
                "codex"
            );
        }
    }
}

/// Full branch menus must fit the runtime's byte budget even when the requested page has 128 refs.
#[test]
fn branch_lazy_pages_fit_the_public_metadata_budget() {
    let mut state = State {
        refs: (0..200)
            .map(|i| format!("refs/heads/feature/topic-{i:03}"))
            .collect(),
        ..Default::default()
    };
    state.trees.insert("branches".into(), handle());
    let root = page(&mut state, None).unwrap();
    let local = root
        .nodes
        .iter()
        .find(|node| node.label == "Local" || node.label == "本地")
        .unwrap();
    let prefixes = page(&mut state, Some(local.id.clone())).unwrap();
    let parent = Some(prefixes.nodes[0].id.clone());
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let children = state
            .tree_page(&tree::Request {
                tree: handle(),
                epoch: 1,
                sequence: 1,
                parent: parent.clone(),
                cursor,
                max_nodes: 128,
            })
            .unwrap();
        assert!(
            serde_json::to_vec(&children).unwrap().len() <= 512 * 1024,
            "branch menu page exceeds runtime byte budget"
        );
        for node in children.nodes {
            assert!(seen.insert(node.label), "a continuation repeated a ref");
        }
        cursor = children.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        seen.len(),
        200,
        "byte-bounded continuations must not omit any refs"
    );
    assert!(seen.contains("topic-199"));
}

fn replacement_page(state: &mut State, _tree_name: &str) -> tree::Page {
    let root = page(state, None).unwrap();
    assert_eq!(root.nodes.len(), 1);
    page(state, Some(root.nodes[0].id.clone())).unwrap()
}

/// Directory and file identities remain disjoint, including when each is paged independently.
#[test]
fn tree_identity_replacement_keeps_branch_and_leaf_distinct() {
    for name in ["staging", "files"] {
        let mut state = replacement(name);
        let nodes = replacement_page(&mut state, name).nodes;
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|node| node.label == "foo"));
        let directory = nodes.iter().find(|node| node.branch).unwrap();
        let file = nodes.iter().find(|node| !node.branch).unwrap();
        assert_ne!(
            directory.id, file.id,
            "{name}: file and directory are separate business nodes"
        );
        let children = page(&mut state, Some(directory.id.clone())).unwrap().nodes;
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].label, "bar");
        assert!(!children[0].branch);
        assert_ne!(children[0].id, file.id);
    }
}

/// The serialized selection of a directory cannot open the equal-spelling deleted file or repaint.
#[test]
fn tree_identity_directory_selected_preserves_scene_and_leaf_selection_works() {
    for name in ["staging", "files"] {
        let mut state = replacement(name);
        let nodes = replacement_page(&mut state, name).nodes;
        let directory = nodes.iter().find(|node| node.branch).unwrap();
        let file = nodes.iter().find(|node| !node.branch).unwrap();
        assert!(
            !state
                .tree_event(tree::Event::Selected {
                    tree: handle(),
                    epoch: 1,
                    node: directory.id.clone(),
                })
                .unwrap(),
            "{name}: a directory must not start a file action"
        );
        STATE.with(|cell| *cell.borrow_mut() = state);
        let invocation = api::Invocation {
            id: 1,
            message: api::Input::Event {
                panel: Some(
                    if name == "files" {
                        "commit-details"
                    } else {
                        "staging"
                    }
                    .into(),
                ),
                event: api::Notification::Ui(ui::UiEvent {
                    revision: 7,
                    node: name.into(),
                    action: ui::Action::Tree(tree::Event::Selected {
                        tree: handle(),
                        epoch: 1,
                        node: directory.id.clone(),
                    }),
                }),
            },
        };
        let completion: api::Completion = serde_json::from_str(
            &GitPlugin::dispatch(serde_json::to_string(&invocation).unwrap()).unwrap(),
        )
        .unwrap();
        let output = completion.result.unwrap();
        let mut state = STATE.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        assert!(
            output.views.is_empty(),
            "{name}: directory selection must preserve pending page authority"
        );
        assert_eq!(state.revision, 7);
        assert_eq!(state.selected_path, "previous.txt");
        assert_eq!(state.history_path, "previous.txt");
        assert!(state.queue.is_empty());
        assert!(
            state
                .tree_event(tree::Event::Selected {
                    tree: handle(),
                    epoch: 1,
                    node: file.id.clone(),
                })
                .unwrap()
        );
        if name == "staging" {
            assert_eq!(state.selected_path, "foo");
        } else {
            assert_eq!(state.history_path, "foo");
        }
    }
}

/// Only the published directory identity can address its children; a leaf is never a parent alias.
#[test]
fn tree_identity_parent_navigation_rejects_leaf_and_unknown_ids() {
    for name in ["staging", "files"] {
        let mut state = replacement(name);
        let nodes = replacement_page(&mut state, name).nodes;
        let directory = nodes.iter().find(|node| node.branch).unwrap();
        let file = nodes.iter().find(|node| !node.branch).unwrap();
        assert!(
            page(&mut state, Some(file.id.clone())).is_err(),
            "{name}: a file cannot navigate into the equal-spelling directory"
        );
        assert!(page(&mut state, Some("unknown".into())).is_err());
        let children = page(&mut state, Some(directory.id.clone())).unwrap().nodes;
        assert_eq!(children[0].label, "bar");
    }
}
