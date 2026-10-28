//! Directory nodes retain stable presentation keys; leaf lookup always uses the original Git path.
use super::*;
use model::Directory;
mod history_files;
pub(super) mod incremental;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod identity_tests;

/// Kind is part of a published path's identity: deleting `foo` while adding `foo/bar` produces
/// both a leaf and a directory named `foo`. Only leaves may trigger Git business actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TreePath {
    Directory(String),
    Leaf(String),
}

impl State {
    /// Lazy pages update their resource only. The returned flag requests a scene only when a
    /// file/ref action changes business state; native directory selection owns no guest state.
    pub(super) fn tree_event(&mut self, event: tree::Event) -> Result<bool, api::Failure> {
        match event {
            tree::Event::Request(request) => {
                let page = self.tree_page(&request);
                tree::reply(request.clone(), page.clone())?;
                if let Ok(page) = page {
                    // Open initial groups once. Explicit expand-all continues through ordinary
                    // bounded lazy pages without bypassing host virtualization or continuation limits.
                    if self.trees.get("staging") == Some(&request.tree) {
                        let expand = self.commit_expand_all
                            || (!self.commit_tree_initialized && request.parent.is_none());
                        if request.parent.is_none() {
                            self.commit_tree_initialized = true;
                        }
                        if expand {
                            tree::set_expanded(
                                request.tree.clone(),
                                page.nodes
                                    .iter()
                                    .filter(|node| node.branch)
                                    .map(|node| node.id.clone())
                                    .collect(),
                                true,
                            )?;
                        }
                    }
                    if self.trees.get("files") == Some(&request.tree)
                        && request.parent.is_none()
                        && !self.history_files_tree_initialized
                    {
                        // Seed the total once; later observations preserve the user's collapse choice.
                        tree::set_expanded(
                            request.tree.clone(),
                            page.nodes.iter().map(|node| node.id.clone()).collect(),
                            true,
                        )?;
                        self.history_files_tree_initialized = true;
                    }
                    self.record_tree_page(&request, page);
                    if self.commit_expand_all && self.trees.get("staging") == Some(&request.tree) {
                        // Expand-all finishes after first pages arrive; later manual disclosure
                        // and provider refreshes must not become permanent automatic expansion.
                        if let Some(pages) = self.tree_pages.get("staging") {
                            self.commit_expand_all = self.tree_paths.iter().any(|(id, path)| {
                                id.starts_with("staging:")
                                    && matches!(path, TreePath::Directory(_))
                                    && !pages.contains_key(&Some(id[8..].to_owned()))
                            });
                        }
                    }
                }
            }
            tree::Event::Checked {
                tree,
                node,
                checked,
                ..
            } => {
                if self.trees.get("staging") == Some(&tree) {
                    return self.commit_check(&node, checked);
                }
            }
            tree::Event::Selected { tree, node, .. } => {
                let name = self
                    .trees
                    .iter()
                    .find(|(_, handle)| **handle == tree)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default();
                let Some(TreePath::Leaf(node)) =
                    self.tree_paths.get(&format!("{name}:{node}")).cloned()
                else {
                    return Ok(false);
                };
                if matches!(name.as_str(), "staging" | "conflicts") && self.ui_busy() {
                    return Ok(false);
                }
                match name.as_str() {
                    "staging" => {
                        if let Some((group, path)) = node.split_once('/') {
                            if self.changes.iter().any(|c| c.path == path) {
                                self.selected_path = path.into();
                                if group == "conflicts" {
                                    self.open_conflict(path)?;
                                } else {
                                    let staged = self.changes.iter().any(|change| {
                                        change.path == path
                                            && change.index != '.'
                                            && change.worktree == '.'
                                    });
                                    self.request_comparison(
                                        panel_activity::ComparisonSelection::Worktree {
                                            path: path.into(),
                                            staged,
                                        },
                                    )?;
                                }
                                return Ok(true);
                            }
                        }
                    }
                    "branches" => {
                        let reference = node
                            .strip_prefix("local/")
                            .map(|s| format!("refs/heads/{s}"))
                            .or_else(|| {
                                node.strip_prefix("remote/")
                                    .map(|s| format!("refs/remotes/{s}"))
                            });
                        if let Some(reference) = reference.filter(|r| self.refs.contains(r)) {
                            if self.filter.as_ref() == Some(&reference) {
                                return Ok(false);
                            }
                            self.select_history_filter(Some(reference))?;
                            return Ok(true);
                        }
                    }
                    "files" => {
                        if !self.history_loading
                            && self
                                .history_preview_commit
                                .as_ref()
                                .is_none_or(|commit| commit.oid == self.selected)
                            && self.history_files.contains(&node)
                        {
                            self.request_comparison(
                                panel_activity::ComparisonSelection::History {
                                    oid: self.selected.clone(),
                                    path: node,
                                },
                            )?;
                            return Ok(true);
                        }
                    }
                    "conflicts" => {
                        if self.changes.iter().any(|c| c.conflict && c.path == node)
                            || self.merge_files.contains_key(&node)
                        {
                            self.open_conflict(&node)?;
                            return Ok(true);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(false)
    }
    /// Derive presentation paths from the current Git observations and retained conflict drafts.
    fn tree_directory(&self, name: &str) -> Directory {
        let mut directory = Directory::default();
        match name {
            "staging" => {
                for change in &self.changes {
                    if change.conflict {
                        directory.insert(&format!("conflicts/{}", change.path));
                    } else {
                        let group = if change.worktree == '?' {
                            "untracked"
                        } else {
                            "changes"
                        };
                        directory.insert(&format!("{group}/{}", change.path));
                    }
                }
            }
            "branches" => {
                // A repository without commits still needs a contextual refresh/fetch entry point.
                directory.children.entry("local".into()).or_default();
                directory.children.entry("remote".into()).or_default();
                for reference in &self.refs {
                    if let Some(tail) = reference.strip_prefix("refs/heads/") {
                        directory.insert(&format!("local/{tail}"));
                    } else if let Some(tail) = reference.strip_prefix("refs/remotes/") {
                        directory.insert(&format!("remote/{tail}"));
                    }
                }
            }
            "files" => {
                for path in &self.history_files {
                    directory.insert(path)
                }
            }
            "conflicts" => {
                for change in self.changes.iter().filter(|c| c.conflict) {
                    directory.insert(&change.path)
                }
                for path in self.merge_files.keys() {
                    directory.insert(path);
                }
            }
            _ => {}
        }
        directory
    }

    /// Return `directory`'s direct children below `parent`, preserving directory/name order except
    /// that the checked-out local branch leads its own level in the `branches` provider.
    /// Both lazy pages and atomic refreshes apply this order before their pagination limits.
    fn tree_children(&self, name: &str, directory: &Directory, parent: &str) -> Vec<TreePath> {
        let key = |part: &str| {
            if parent.is_empty() {
                part.to_owned()
            } else {
                format!("{parent}/{part}")
            }
        };
        let mut paths: Vec<_> = directory
            .children
            .keys()
            .map(|part| TreePath::Directory(key(part)))
            .chain(
                directory
                    .files
                    .iter()
                    .map(|(part, _)| TreePath::Leaf(key(part))),
            )
            .collect();
        if name == "branches" {
            if let Some(reference) = self.current_ref.strip_prefix("refs/heads/") {
                let current = TreePath::Leaf(format!("local/{reference}"));
                if let Some(index) = paths.iter().position(|path| path == &current) {
                    let current = paths.remove(index);
                    paths.insert(0, current);
                }
            }
        }
        paths
    }

    /// Derive one node from its stable identity/path for both lazy pages and cached presentation
    /// updates. Only the latter's non-structural fields may change without a source refresh.
    pub(super) fn provider_node(&self, name: &str, id: &str, path: &TreePath) -> tree::Node {
        let (path, directory) = match path {
            TreePath::Directory(path) => (path.as_str(), true),
            TreePath::Leaf(path) => (path.as_str(), false),
        };
        let current = name == "branches"
            && !directory
            && path
                .strip_prefix("local/")
                .is_some_and(|tail| format!("refs/heads/{tail}") == self.current_ref);
        let part = path.rsplit('/').next().unwrap_or(path);
        // Only synthetic roots are localized; real nested directories keep their literal names.
        let label = if directory && !path.contains('/') {
            match part {
                "conflicts" => self.label("冲突", "Conflicts"),
                "staged" => self.label("已暂存", "Staged"),
                "unstaged" => self.label("未暂存", "Unstaged"),
                "changes" => self.label("更改", "Changes"),
                "untracked" => self.label("未进行版本管理的文件", "Unversioned Files"),
                "local" => self.label("本地", "Local"),
                "remote" => self.label("远程", "Remote"),
                _ => part,
            }
        } else {
            part
        };
        let mut node = tree::Node::new(id, label, directory);
        if name == "staging" {
            let tree_path = if directory {
                TreePath::Directory(path.into())
            } else {
                TreePath::Leaf(path.into())
            };
            let changes = self.commit_changes(&tree_path);
            node.checked = self.commit_selection_state(&changes);
            if directory {
                node.suffix = format!("{} {}", changes.len(), self.label("个文件", "files"));
            } else if let Some(change) = changes.first() {
                let status = if change.index != '.' {
                    change.index
                } else {
                    change.worktree
                };
                node.tone = match status {
                    'A' | '?' => Some(tree::Tone::Positive),
                    'D' => Some(tree::Tone::Negative),
                    'M' | 'R' | 'C' => Some(tree::Tone::Informative),
                    _ => None,
                };
                node.description = if node.checked == Some(tree::CheckState::Mixed) {
                    self.label(
                        "部分暂存；勾选以暂存剩余改动",
                        "Partially staged; check to stage remaining changes",
                    )
                    .into()
                } else {
                    change.path.clone()
                };
            }
        }
        if name == "files" && !directory {
            node.tone = self
                .history_file_status
                .get(path)
                .map(|change| match change {
                    model::HistoryChange::Added => tree::Tone::Positive,
                    model::HistoryChange::Deleted => tree::Tone::Negative,
                    model::HistoryChange::Modified => tree::Tone::Informative,
                });
            if let Some(change) = self.history_file_status.get(path) {
                node.description = match change {
                    model::HistoryChange::Added => self.label("新增", "Added"),
                    model::HistoryChange::Deleted => self.label("删除", "Deleted"),
                    model::HistoryChange::Modified => self.label("修改", "Modified"),
                }
                .into();
            }
        }
        if name == "branches" {
            node.icon = Some(if directory {
                ui::ToolIcon::new("icons/folder.svg", "icons/folder-dark.svg")
            } else if current {
                ui::ToolIcon::new("icons/current-branch.svg", "icons/current-branch-dark.svg")
            } else {
                ui::ToolIcon::new("icons/git.svg", "icons/git.svg")
            });
            let reference = if directory {
                String::new()
            } else {
                self.branch_reference(path)
            };
            node.action_menu = self.branch_actions(&reference);
        }
        if name == "conflicts"
            && !directory
            && self.merge_files.get(path).is_some_and(|file| file.resolved)
        {
            node.description = self.label("已解决", "Resolved").into();
        }
        if current {
            node.description = self.label("当前分支", "Current branch").into();
        }
        node
    }

    /// Build one lazy page before its public resource reply, recording only its published paths.
    /// Unrequested siblings must not expand the work or payload of future metadata updates.
    fn tree_page(&mut self, request: &tree::Request) -> Result<tree::Page, api::Failure> {
        let name = self
            .trees
            .iter()
            .find(|(_, handle)| **handle == request.tree)
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        let directory = self.tree_directory(&name);
        let parent = match request.parent.as_ref() {
            None => String::new(),
            Some(id) => match self.tree_paths.get(&format!("{name}:{id}")) {
                Some(TreePath::Directory(path)) => path.clone(),
                _ => return Err(failure("Directory expired")),
            },
        };
        let mut current = &directory;
        for part in parent.split('/').filter(|s| !s.is_empty()) {
            if let Some(child) = current.children.get(part) {
                current = child
            } else {
                return Err(failure("Directory expired"));
            }
        }
        let paths = self.tree_entries(&name, current, &parent, request.parent.is_none());
        let start = request
            .cursor
            .as_deref()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let count = request.max_nodes.min(128) as usize;
        let total = paths.len();
        let mut nodes = Vec::new();
        // Full typed context menus can exhaust bytes before the 128-node limit. Reserve space
        // for the Page envelope/cursor and advance by published nodes only, never dropping a ref.
        let mut bytes = 1024;
        for entry in paths.into_iter().skip(start).take(count) {
            let node = entry.node(self, &name);
            let size = serde_json::to_vec(&node)
                .map_err(|error| failure(error.to_string()))?
                .len()
                + 1;
            if bytes + size > 512 * 1024 {
                if nodes.is_empty() {
                    return Err(api::Failure::new(
                        api::ErrorCode::LimitExceeded,
                        "Tree node exceeds the page byte budget",
                    ));
                }
                break;
            }
            bytes += size;
            self.tree_paths
                .insert(format!("{name}:{}", node.id), entry.path);
            nodes.push(node);
        }
        let end = start + nodes.len();
        Ok(tree::Page {
            nodes,
            next: (end < total).then(|| end.to_string()),
        })
    }

    pub(super) fn label<'a>(&self, zh: &'a str, en: &'a str) -> &'a str {
        if self.environment.locale.starts_with("en") {
            en
        } else {
            zh
        }
    }
}

/// Tree identities are bounded hashes, independent from a potentially long or control-bearing path.
pub(super) fn node_id(path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::hash::DefaultHasher::new();
    path.hash(&mut hash);
    format!("n{:016x}", hash.finish())
}

/// Convert a full `reference` under refs/heads into exact root-to-leaf IDs without filtering the log.
/// Detached/unborn HEAD has no invented ref; over-deep paths cannot exceed the native tree quota.
pub(super) fn branch_initial_path(reference: &str) -> Option<Vec<String>> {
    let reference = reference.strip_prefix("refs/heads/")?;
    let path = format!("local/{reference}");
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() > 64 {
        return None;
    }
    Some(
        (1..=parts.len())
            .map(|end| {
                let id = node_id(&parts[..end].join("/"));
                if end == parts.len() {
                    id
                } else {
                    format!("d{id}")
                }
            })
            .collect(),
    )
}
