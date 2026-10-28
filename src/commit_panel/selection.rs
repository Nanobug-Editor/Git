//! Commit ranges are local choices; only explicit submit or staging actions may write Git's index.
use super::*;

impl State {
    /// Aggregate local inclusion for a group, directory or file; unloaded descendants participate too.
    /// Conflicts have no checkbox and never become an implicit submission choice.
    pub(crate) fn commit_selection_state(&self, changes: &[&Change]) -> Option<tree::CheckState> {
        if changes.is_empty() || changes.iter().any(|change| change.conflict) {
            return None;
        }
        let selected = changes
            .iter()
            .filter(|change| self.commit_selected.contains(&change.path))
            .count();
        Some(if selected == 0 {
            tree::CheckState::Unchecked
        } else if selected == changes.len() {
            tree::CheckState::Checked
        } else {
            tree::CheckState::Mixed
        })
    }

    /// Apply one checkbox gesture to every descendant without Git, native buffer checks or row focus.
    /// An accepted submission owns a frozen range; later gestures can only affect a subsequent submit.
    pub(crate) fn commit_check(&mut self, node: &str, checked: bool) -> Result<bool, api::Failure> {
        let Some(path) = self.tree_paths.get(&format!("staging:{node}")).cloned() else {
            return Ok(false);
        };
        let paths = self
            .commit_changes(&path)
            .into_iter()
            .filter(|change| !change.conflict)
            .map(|change| change.path.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for path in paths {
            changed |= if checked {
                self.commit_selected.insert(path)
            } else {
                self.commit_selected.remove(&path)
            };
        }
        if changed {
            self.publish_commit_selection()?;
        }
        Ok(changed)
    }

    /// Inclusion cannot change labels, icons, membership or paging. Patch only cached checkbox
    /// metadata and keep every accepted native branch instead of rebuilding the repository tree.
    fn publish_commit_selection(&mut self) -> Result<(), api::Failure> {
        let Some(handle) = self.trees.get("staging").cloned() else {
            return Ok(());
        };
        let patch = self.commit_selection_patch();
        if patch.nodes.is_empty() {
            return Ok(());
        }
        let accepted = patch
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.checked))
            .collect::<BTreeMap<_, _>>();
        tree::apply_patch(handle, patch)?;
        if let Some(pages) = self.tree_pages.get_mut("staging") {
            for node in pages.values_mut().flat_map(|page| &mut page.nodes) {
                if let Some(checked) = accepted.get(&node.id) {
                    node.checked = *checked;
                }
            }
        }
        Ok(())
    }

    /// Count full-directory inclusion once, including unloaded descendants, then visit cached rows.
    /// Work is proportional to path depth and cached nodes, rather than files times cached rows.
    pub(super) fn commit_selection_patch(&self) -> tree::Patch {
        let mut patch = tree::Patch::default();
        let Some(pages) = self.tree_pages.get("staging") else {
            return patch;
        };
        // Git can report a deleted file "foo" alongside a new "foo/bar". Keep kind in the key
        // so that selecting the directory cannot make the deleted file's checkbox mixed/checked.
        let mut counts = BTreeMap::<(bool, String), (usize, usize)>::new();
        for change in self.changes.iter().filter(|change| !change.conflict) {
            let group = if change.worktree == '?' {
                "untracked"
            } else {
                "changes"
            };
            let selected = usize::from(self.commit_selected.contains(&change.path));
            let path = format!("{group}/{}", change.path);
            counts.insert((false, path.clone()), (1, selected));
            for (end, _) in path.match_indices('/') {
                let count = counts.entry((true, path[..end].into())).or_default();
                count.0 += 1;
                count.1 += selected;
            }
        }
        for node in pages.values().flat_map(|page| &page.nodes) {
            if node.checked.is_none() {
                continue;
            }
            let Some(TreePath::Directory(path) | TreePath::Leaf(path)) =
                self.tree_paths.get(&format!("staging:{}", node.id))
            else {
                continue;
            };
            let Some(&(total, included)) = counts.get(&(node.branch, path.clone())) else {
                continue;
            };
            let checked = Some(if included == 0 {
                tree::CheckState::Unchecked
            } else if included == total {
                tree::CheckState::Checked
            } else {
                tree::CheckState::Mixed
            });
            if node.checked != checked {
                let mut updated = node.clone();
                updated.checked = checked;
                patch.nodes.push(updated);
            }
        }
        patch
    }

    /// Retire choices only when an authoritative refresh no longer contains their file.
    /// Index changes or movement between tracked/unversioned groups do not reset local inclusion.
    pub(crate) fn retain_commit_selection(&mut self) {
        self.commit_selected.retain(|path| {
            self.changes
                .iter()
                .any(|change| change.path == *path && !change.conflict)
        });
    }
}
