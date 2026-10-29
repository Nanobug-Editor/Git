//! Provider-side cached-prefix diffs use the public atomic tree patch, retaining unrelated pages.
use super::*;

/// Only accepted provider metadata is cached; native expansion, focus, selection and scroll stay host-owned.
#[derive(Clone, Default)]
pub(crate) struct CachedBranch {
    /// The complete loaded prefix, including all successful continuation pages.
    pub nodes: Vec<tree::Node>,
    /// The current provider continuation, absent after the end of this directory.
    pub next: Option<String>,
}

impl State {
    /// Record a successfully accepted reply, never a merely generated or rejected lazy page.
    pub(crate) fn record_tree_page(&mut self, request: &tree::Request, page: tree::Page) {
        let Some(name) = self
            .trees
            .iter()
            .find_map(|(name, handle)| (handle == &request.tree).then_some(name.clone()))
        else {
            return;
        };
        let cached = self
            .tree_pages
            .entry(name)
            .or_default()
            .entry(request.parent.clone())
            .or_default();
        if request.cursor.is_none() {
            cached.nodes.clear();
        }
        cached.nodes.extend(page.nodes);
        cached.next = page.next;
    }

    /// Publish only changed nodes/child prefixes; an unchanged Git observation sends no tree call.
    /// Failed admission preserves the host's complete tree and the provider's previous baseline.
    pub(crate) fn reconcile_tree(&mut self, name: &str) -> Result<(), api::Failure> {
        let Some(handle) = self.trees.get(name).cloned() else {
            return Ok(());
        };
        let (patch, pages, paths) = self.tree_patch(name)?;
        if patch.nodes.is_empty() && patch.branches.is_empty() {
            return Ok(());
        }
        tree::apply_patch(handle, patch)?;
        self.tree_pages.insert(name.into(), pages);
        self.tree_paths
            .retain(|key, _| !key.starts_with(&format!("{name}:")));
        self.tree_paths.extend(
            paths
                .into_iter()
                .map(|(id, path)| (format!("{name}:{id}"), path)),
        );
        if name == "files" && !self.history_files_tree_initialized {
            tree::set_expanded(
                self.trees[name].clone(),
                vec![format!("d{}", node_id(""))],
                true,
            )?;
            self.history_files_tree_initialized = true;
        }
        Ok(())
    }

    /// Publish all immutable history metadata; mutable providers retain their loaded window.
    /// Stable IDs and both publication modes obey the same atomic patch and retained-tree quotas.
    fn tree_patch(
        &self,
        name: &str,
    ) -> Result<
        (
            tree::Patch,
            BTreeMap<Option<String>, CachedBranch>,
            BTreeMap<String, TreePath>,
        ),
        api::Failure,
    > {
        let mut patch = tree::Patch::default();
        // Historical metadata is already complete and immutable. Admit its entire forest once,
        // while mutable status/ref providers continue preserving their requested page windows.
        let complete = name == "files";
        let empty = BTreeMap::new();
        let previous = match self.tree_pages.get(name) {
            Some(previous) => previous,
            None if complete => &empty,
            None => return Ok((patch, BTreeMap::new(), BTreeMap::new())),
        };
        let directory = self.tree_directory(name);
        let mut pages = BTreeMap::new();
        let mut paths: BTreeMap<String, TreePath> = BTreeMap::new();
        // A compact terminal directory can move to a different visible parent without changing ID.
        // Visit only the new reachable hierarchy, so an orphaned old parent cannot emit it a second time.
        let mut pending = VecDeque::from([None]);
        while let Some(parent) = pending.pop_front() {
            let unloaded = CachedBranch::default();
            let cached = match previous.get(&parent) {
                Some(cached) => cached,
                None if complete => &unloaded,
                None => continue,
            };
            let prefix = match &parent {
                None => String::new(),
                Some(id) => match paths.get(id) {
                    Some(TreePath::Directory(path)) => path.clone(),
                    _ => continue,
                },
            };
            let mut current = &directory;
            let mut present = true;
            for part in prefix.split('/').filter(|part| !part.is_empty()) {
                if let Some(child) = current.children.get(part) {
                    current = child;
                } else {
                    present = false;
                    break;
                }
            }
            if !present {
                continue;
            }
            let candidates = self.tree_entries(name, current, &prefix, parent.is_none());
            // Keep the loaded window size. A newly enlarged complete directory starts another
            // ordinary bounded page rather than materializing every new path in the repository.
            let limit = if complete {
                usize::MAX
            } else if cached.next.is_some() {
                cached.nodes.len().max(1)
            } else {
                cached.nodes.len().max(128)
            };
            let mut nodes = Vec::new();
            for entry in candidates.iter().take(limit) {
                if paths.len() >= tree::MAX_NODES {
                    return Err(api::Failure::new(
                        api::ErrorCode::LimitExceeded,
                        "Complete changed-file tree exceeds the native node budget",
                    ));
                }
                let node = entry.node(self, name);
                paths.insert(node.id.clone(), entry.path.clone());
                nodes.push(node);
            }
            let next = (nodes.len() < candidates.len()).then(|| nodes.len().to_string());
            pending.extend(
                nodes
                    .iter()
                    .filter(|node| node.branch)
                    .map(|node| Some(node.id.clone())),
            );
            let old = cached
                .nodes
                .iter()
                .map(|node| (&node.id, node))
                .collect::<BTreeMap<_, _>>();
            patch.nodes.extend(
                nodes
                    .iter()
                    .filter(|node| old.get(&node.id).is_none_or(|previous| *previous != *node))
                    .cloned(),
            );
            let children = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
            if !previous.contains_key(&parent)
                || children
                    != cached
                        .nodes
                        .iter()
                        .map(|node| node.id.clone())
                        .collect::<Vec<_>>()
                || next != cached.next
            {
                patch.branches.push(tree::BranchPatch {
                    parent: parent.clone(),
                    children,
                    next: next.clone(),
                });
            }
            pages.insert(parent.clone(), CachedBranch { nodes, next });
        }
        // A new prefix may push a formerly expanded directory beyond the loaded page. Retire
        // that orphan's cached descendants rather than trying to attach them without its parent.
        let mut reachable = std::collections::BTreeSet::new();
        let mut queue = VecDeque::from([None]);
        while let Some(parent) = queue.pop_front() {
            if let Some(page) = pages.get(&parent) {
                for node in &page.nodes {
                    if reachable.insert(node.id.clone()) && node.branch {
                        queue.push_back(Some(node.id.clone()));
                    }
                }
            }
        }
        patch.nodes.retain(|node| reachable.contains(&node.id));
        patch.branches.retain(|branch| {
            branch
                .parent
                .as_ref()
                .is_none_or(|id| reachable.contains(id))
        });
        pages.retain(|parent, _| parent.as_ref().is_none_or(|id| reachable.contains(id)));
        paths.retain(|id, _| reachable.contains(id));
        patch.validate()?;
        Ok((patch, pages, paths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A commit's paths are already complete: collapsed folders must not require more guest turns.
    #[test]
    fn history_tree_complete_publication_includes_unexpanded_descendants() {
        let mut state = State {
            history_files: (0..220)
                .map(|index| format!("src/nested/file-{index:03}.rs"))
                .chain(["docs/readme.md".into()])
                .collect(),
            ..Default::default()
        };
        let handle = api::ResourceHandle {
            instance: "fixture".into(),
            scope: "workspace".into(),
            resource: 1,
        };
        state.trees.insert("files".into(), handle.clone());
        let request = tree::Request {
            tree: handle,
            epoch: 1,
            sequence: 1,
            parent: None,
            cursor: None,
            max_nodes: 128,
        };
        let root = state.tree_page(&request).unwrap();
        state.record_tree_page(&request, root);
        let (patch, pages, paths) = state.tree_patch("files").unwrap();
        assert_eq!(
            paths
                .values()
                .filter(|path| matches!(path, TreePath::Leaf(_)))
                .count(),
            221
        );
        assert!(pages.values().all(|page| page.next.is_none()));
        assert_eq!(
            pages[&Some(format!("d{}", node_id("src/nested")))]
                .nodes
                .len(),
            220
        );
        patch.validate().unwrap();
        state.tree_pages.insert("files".into(), pages);
        state.tree_paths = paths
            .into_iter()
            .map(|(id, path)| (format!("files:{id}"), path))
            .collect();
        let cached = state.history_navigation_files();
        assert!(!state.tree_event(tree::Event::Request(request)).unwrap());
        assert_eq!(
            state.history_navigation_files(),
            cached,
            "late root input cannot trim the complete forest"
        );
    }

    /// Provider navigation follows visible directory-first order, including compact ancestors.
    #[test]
    fn history_tree_complete_navigation_has_full_order_and_exact_reveal_path() {
        let mut state = State {
            history_files: vec![
                "a.txt".into(),
                "src/nested/a.rs".into(),
                "src/nested/b.rs".into(),
            ],
            ..Default::default()
        };
        let (_, pages, paths) = state.tree_patch("files").unwrap();
        state.tree_pages.insert("files".into(), pages);
        state.tree_paths.extend(
            paths
                .into_iter()
                .map(|(id, path)| (format!("files:{id}"), path)),
        );
        assert_eq!(
            state.history_navigation_files(),
            ["src/nested/a.rs", "src/nested/b.rs", "a.txt"]
        );
        assert_eq!(
            state.history_reveal_ids("src/nested/b.rs").unwrap(),
            [
                format!("d{}", node_id("")),
                format!("d{}", node_id("src/nested")),
                node_id("src/nested/b.rs")
            ]
        );
        assert!(state.history_reveal_ids("missing.rs").is_err());
    }

    /// Oversized complete publications fail explicitly rather than silently keeping a partial prefix.
    #[test]
    fn history_tree_complete_publication_rejects_excess_nodes() {
        let state = State {
            history_files: (0..tree::MAX_NODES)
                .map(|index| format!("file-{index}.txt"))
                .collect(),
            ..Default::default()
        };
        let error = state
            .tree_patch("files")
            .err()
            .expect("total plus leaves exceeds quota");
        assert_eq!(error.code, api::ErrorCode::LimitExceeded);
    }

    /// Simulate successful ordinary provider replies without introducing a test-only SDK transport.
    fn load(state: &mut State, name: &str) {
        let handle = api::ResourceHandle {
            instance: "fixture".into(),
            scope: "workspace".into(),
            resource: 1,
        };
        state.trees.insert(name.into(), handle.clone());
        let mut queue = VecDeque::from([None]);
        while let Some(parent) = queue.pop_front() {
            let request = tree::Request {
                tree: handle.clone(),
                epoch: 1,
                sequence: 1,
                parent,
                cursor: None,
                max_nodes: 128,
            };
            let page = state.tree_page(&request).unwrap();
            queue.extend(
                page.nodes
                    .iter()
                    .filter(|node| node.branch)
                    .map(|node| Some(node.id.clone())),
            );
            state.record_tree_page(&request, page);
        }
    }

    /// Checkout updates only the old/new sibling lists and current marker; selecting history
    /// alone changes neither branch order nor the checked-out identity.
    #[test]
    fn incremental_branch_checkout_reorders_own_levels_and_retains_remote_page() {
        let mut state = State {
            current_ref: "refs/heads/main".into(),
            refs: vec![
                "refs/heads/main".into(),
                "refs/heads/codex/a-first".into(),
                "refs/heads/codex/z-topic".into(),
                "refs/remotes/origin/main".into(),
            ],
            ..Default::default()
        };
        load(&mut state, "branches");
        state.filter = Some("refs/heads/codex/z-topic".into());
        assert_eq!(
            state.tree_patch("branches").unwrap().0,
            tree::Patch::default()
        );
        state.current_ref = "refs/heads/codex/z-topic".into();
        let (patch, pages, _) = state.tree_patch("branches").unwrap();
        assert_eq!(patch.branches.len(), 2);
        assert!(patch.branches.iter().all(|branch| {
            ["local", "local/codex"]
                .iter()
                .any(|path| branch.parent == Some(format!("d{}", node_id(path))))
        }));
        let nested = &pages[&Some(format!("d{}", node_id("local/codex")))];
        assert_eq!(nested.nodes[0].label, "z-topic");
        assert!(
            nested.nodes[0]
                .icon
                .as_ref()
                .unwrap()
                .light
                .contains("current-branch")
        );
        let remote = &pages[&Some(format!("d{}", node_id("remote/origin")))];
        assert_eq!(
            remote.nodes[0].icon,
            Some(ui::ToolIcon::new("icons/git.svg", "icons/git.svg"))
        );
        assert_eq!(
            pages[&Some(format!("d{}", node_id("local")))].nodes[0].label,
            "codex"
        );
    }

    /// One added dirty path changes one folder; status bytes with unchanged presentation send no delta.
    #[test]
    fn incremental_tree_plan_preserves_unaffected_loaded_prefixes() {
        let mut state = State {
            changes: vec![
                Change {
                    path: "src/old.rs".into(),
                    index: '.',
                    worktree: 'M',
                    ..Default::default()
                },
                Change {
                    path: "docs/readme.md".into(),
                    index: '.',
                    worktree: 'M',
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        load(&mut state, "staging");
        assert_eq!(
            state.tree_patch("staging").unwrap().0,
            tree::Patch::default()
        );
        state.changes[0].index_entry = b"different content, same tree membership".to_vec();
        assert_eq!(
            state.tree_patch("staging").unwrap().0,
            tree::Patch::default()
        );
        state.changes.push(Change {
            path: "src/new.rs".into(),
            index: '.',
            worktree: 'M',
            ..Default::default()
        });
        let (patch, _, _) = state.tree_patch("staging").unwrap();
        // The leaf plus its two ancestor counts change; the docs branch remains untouched.
        assert_eq!(patch.nodes.len(), 3);
        assert!(patch.nodes.iter().any(|node| node.label == "new.rs"));
        assert!(!patch.nodes.iter().any(|node| node.label == "docs"));
        assert_eq!(patch.branches.len(), 1);
        assert_eq!(
            patch.branches[0].parent,
            Some(format!("d{}", node_id("changes/src")))
        );
    }

    /// Compact rows retain one identity even when a new commit removes their former visible parent.
    #[test]
    fn compact_history_directory_can_move_to_a_different_visible_parent() {
        let mut state = State {
            environment: plugin_protocol::Environment {
                locale: "en".into(),
                ..Default::default()
            },
            history_files: [
                "crates/editor-app/src/a.rs",
                "crates/editor-app/src/b.rs",
                "crates/editor-ui/theme.rs",
                "README.md",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            ..Default::default()
        };
        load(&mut state, "files");
        state.history_files = vec!["crates/editor-app/src/b.rs".into()];
        let (patch, _, _) = state
            .tree_patch("files")
            .expect("a moved compact directory must have one current presentation");
        patch.validate().unwrap();
        let moved = patch
            .nodes
            .iter()
            .find(|node| node.label == "crates\\editor-app\\src")
            .unwrap();
        assert_eq!(moved.suffix, "1 file");
    }

    /// A path changing status only keeps children and pagination, updating just its semantic color.
    #[test]
    fn incremental_tree_plan_updates_file_tones_without_structure_reload() {
        let mut state = State {
            history_files: vec!["a.txt".into()],
            history_file_status: BTreeMap::from([("a.txt".into(), model::HistoryChange::Added)]),
            ..Default::default()
        };
        load(&mut state, "files");
        state
            .history_file_status
            .insert("a.txt".into(), model::HistoryChange::Deleted);
        let (patch, _, _) = state.tree_patch("files").unwrap();
        assert!(patch.branches.is_empty());
        assert_eq!(patch.nodes.len(), 1);
        assert_eq!(patch.nodes[0].tone, Some(tree::Tone::Negative));
    }

    /// Existing partial history pages become complete, so scrolling and expanding need no replies.
    #[test]
    fn history_tree_complete_publication_finishes_existing_partial_pages() {
        let mut state = State {
            history_files: (0..200).map(|i| format!("file-{i:03}.txt")).collect(),
            ..Default::default()
        };
        let handle = api::ResourceHandle {
            instance: "fixture".into(),
            scope: "workspace".into(),
            resource: 1,
        };
        state.trees.insert("files".into(), handle.clone());
        let mut request = tree::Request {
            tree: handle,
            epoch: 1,
            sequence: 1,
            parent: None,
            cursor: None,
            max_nodes: 10,
        };
        let page = state.tree_page(&request).unwrap();
        request.parent = Some(page.nodes[0].id.clone());
        state.record_tree_page(
            &tree::Request {
                parent: None,
                ..request.clone()
            },
            page,
        );
        let page = state.tree_page(&request).unwrap();
        state.record_tree_page(&request, page);
        state.history_files.insert(0, "a-new.txt".into());
        let (patch, pages, _) = state.tree_patch("files").unwrap();
        assert_eq!(pages[&request.parent].nodes.len(), 201);
        assert_eq!(pages[&request.parent].next, None);
        assert_eq!(patch.nodes.len(), 192);
        assert_eq!(patch.branches[0].children.len(), 201);
    }
}
