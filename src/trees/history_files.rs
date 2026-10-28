//! Compact history rows retain terminal directory identities and original leaf paths across refreshes.
use super::*;

/// Cheap candidate metadata precedes paging; typed menus and leaf styling are built only if loaded.
pub(super) struct TreeEntry {
    pub path: TreePath,
    /// History directories already carry their relative compact label and complete descendant count.
    presentation: Option<tree::Node>,
}

impl TreeEntry {
    /// Node kind remains part of identity, including the synthetic root's empty directory path.
    pub(super) fn node(&self, state: &State, name: &str) -> tree::Node {
        self.presentation.clone().unwrap_or_else(|| {
            let id = match &self.path {
                TreePath::Directory(path) => format!("d{}", node_id(path)),
                TreePath::Leaf(path) => node_id(path),
            };
            state.provider_node(name, &id, &self.path)
        })
    }
}

impl State {
    /// One projection serves lazy replies and cached patches so their IDs, labels and counts agree.
    /// An empty directory path identifies the synthetic history total; real Git paths are nonempty.
    pub(super) fn tree_entries(
        &self,
        name: &str,
        directory: &Directory,
        parent: &str,
        root_page: bool,
    ) -> Vec<TreeEntry> {
        if name != "files" {
            return self
                .tree_children(name, directory, parent)
                .into_iter()
                .map(|path| TreeEntry {
                    path,
                    presentation: None,
                })
                .collect();
        }
        if root_page {
            let mut node = tree::Node::new(
                format!("d{}", node_id("")),
                self.history_file_count(directory),
                true,
            );
            node.muted = true;
            return vec![TreeEntry {
                path: TreePath::Directory(String::new()),
                presentation: Some(node),
            }];
        }
        let key = |part: &str| {
            if parent.is_empty() {
                part.to_owned()
            } else {
                format!("{parent}/{part}")
            }
        };
        let mut nodes = Vec::new();
        for (part, child) in &directory.children {
            let mut path = key(part);
            let mut label = part.clone();
            let mut terminal = child;
            // Stop at every branch or direct file. A same-named deleted leaf remains independent.
            while terminal.files.is_empty() && terminal.children.len() == 1 {
                let (part, child) = terminal.children.first_key_value().unwrap();
                // Keep every row admissible even when a long real path spans many small segments.
                if label.len() + 1 + part.len() > 1024 {
                    break;
                }
                path.push('/');
                path.push_str(part);
                label.push('\\');
                label.push_str(part);
                terminal = child;
            }
            let id = format!("d{}", node_id(&path));
            let mut node = tree::Node::new(id, label, true);
            node.suffix = self.history_file_count(terminal);
            node.description = path.clone();
            nodes.push(TreeEntry {
                path: TreePath::Directory(path),
                presentation: Some(node),
            });
        }
        for (_, path) in &directory.files {
            nodes.push(TreeEntry {
                path: TreePath::Leaf(path.clone()),
                presentation: None,
            });
        }
        nodes
    }

    /// Counts include unloaded descendants and unique original paths, independently of pagination.
    fn history_file_count(&self, directory: &Directory) -> String {
        fn count(directory: &Directory) -> usize {
            let mut pending = vec![directory];
            let mut files = 0;
            while let Some(directory) = pending.pop() {
                files += directory.files.len();
                pending.extend(directory.children.values());
            }
            files
        }
        let count = count(directory);
        format!(
            "{} {}",
            count,
            self.label("个文件", if count == 1 { "file" } else { "files" })
        )
    }
}
