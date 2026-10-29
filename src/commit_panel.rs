//! Compact commit presentation and checkbox policy; the host owns native tree/input behavior.
use super::*;
use trees::TreePath;

mod selection;
mod submission;
#[cfg(test)]
mod tests;

impl State {
    /// Keep the file viewport above a stationary message/completion area, including narrow sidebars.
    pub(super) fn commit_panel(&self) -> ui::Node {
        let selected = self
            .changes
            .iter()
            .find(|change| change.path == self.selected_path);
        let busy = self.ui_busy();
        let available = !busy && self.repository_available();
        let editable = available && selected.is_some_and(|change| !change.conflict);
        let toolbar = ui::Node::row(
            "commit-toolbar",
            vec![
                self.commit_tool(
                    "refresh",
                    "刷新",
                    "Refresh",
                    "M4 7a7 7 0 1 1-1 8M4 3v4h4",
                    !busy,
                ),
                self.commit_tool(
                    "discard",
                    "回滚所选文件…",
                    "Revert selected file…",
                    "M9 4 4 9l5 5M4 9h9a5 5 0 0 1 0 10",
                    editable && selected.is_some_and(|c| c.worktree != '.'),
                ),
                self.commit_tool(
                    "stage",
                    "暂存所选文件",
                    "Stage selected file",
                    "M12 4v16M4 12h16",
                    editable && selected.is_some_and(|c| c.worktree != '.'),
                ),
                self.commit_tool(
                    "unstage",
                    "取消暂存所选文件",
                    "Unstage selected file",
                    "M4 12h16",
                    editable && selected.is_some_and(|c| c.index != '.'),
                ),
                self.commit_tool(
                    "diff",
                    "显示工作区差异",
                    "Show worktree difference",
                    "M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12ZM9 12a3 3 0 1 0 6 0 3 3 0 1 0-6 0",
                    editable,
                ),
                self.commit_tool(
                    "commit-hunks",
                    "暂存或取消暂存差异块…",
                    "Stage or unstage hunks…",
                    "M5 4h14v16H5ZM8 8h8M8 12h8M8 16h5",
                    editable && !self.patch.hunks.is_empty(),
                ),
                self.commit_tool(
                    "commit-expand",
                    "全部展开",
                    "Expand all",
                    "m7 10 5-5 5 5M7 14l5 5 5-5",
                    !busy && !self.changes.is_empty(),
                ),
                self.commit_tool(
                    "commit-collapse",
                    "全部折叠",
                    "Collapse all",
                    "m7 5 5 5 5-5M7 19l5-5 5 5",
                    !busy && !self.changes.is_empty(),
                ),
            ],
        )
        .gap(2.)
        .padding(4.)
        .wrap();
        // Both actions consume the same local range. A checkbox never waits for an index writer.
        let can_submit = self.can_submit_commit();
        let footer = vec![
            self.input(
                "commit-message",
                &self.commit_message,
                self.label("提交消息", "Commit message"),
                true,
            )
            .textarea_rows(2, 4),
            ui::Node::row(
                "commit-actions",
                vec![
                    self.button("commit", "提交", "Commit")
                        .role("primary_button")
                        .disabled(!can_submit),
                    self.button("commit-push", "提交并推送", "Commit and Push")
                        .disabled(!can_submit),
                ],
            )
            .gap(6.)
            .wrap(),
        ];
        let composer = ui::Node::column("commit-composer", footer)
            .gap(8.)
            .padding(10.);
        let mut divider = ui::Node::new("commit-divider", ui::Kind::Separator);
        divider.layout.margin = ui::Edges {
            left: 10.,
            right: 10.,
            ..Default::default()
        };
        ui::Node::column(
            "staging-root",
            vec![
                toolbar,
                // Range choices, disclosure and scrolling outlive the frozen submit command.
                // Only its two submit actions are locked while Git processes that captured range.
                self.tree_node("staging").disabled(false),
                divider,
                composer,
            ],
        )
        .gap(0.)
        .grow()
    }

    /// An icon keeps its localized name as tooltip and native accessibility text.
    fn commit_tool(&self, id: &str, zh: &str, en: &str, path: &str, enabled: bool) -> ui::Node {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><path d=\"{path}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>"
        );
        self.button(id, zh, en)
            .icon(svg)
            .role("toolbar_button")
            .tooltip(self.label(zh, en))
            .disabled(!enabled)
    }

    /// Existing per-hunk actions remain accessible without growing the permanent commit form.
    pub(super) fn commit_hunk_dialog(&self) -> Option<ui::Dialog> {
        self.commit_hunks_open.then(|| {
            let mut rows = vec![ui::Node::text("selected-path", &self.selected_path)];
            rows.extend(self.patch.hunks.iter().enumerate().map(|(index, _)| {
                let caption = format!(
                    "{} {}",
                    if self.patch.staged {
                        self.label("取消暂存差异块", "Unstage hunk")
                    } else {
                        self.label("暂存差异块", "Stage hunk")
                    },
                    index + 1
                );
                self.button(&format!("hunk-{index}"), &caption, &caption)
            }));
            ui::Dialog {
                id: "commit-hunks-dialog".into(),
                title: self.label("差异块操作", "Hunk actions").into(),
                width: 420.,
                content: ui::Node::column(
                    "commit-hunks-body",
                    vec![
                        ui::Node::scroll(
                            "commit-hunks-scroll",
                            ui::Node::column("hunk-actions", rows),
                        )
                        .height(260.),
                        self.button("commit-hunks-close", "关闭", "Close"),
                    ],
                ),
            }
        })
    }

    /// Handle presentation controls only; repository mutations continue through existing admission.
    pub(super) fn commit_panel_event(&mut self, event: &ui::UiEvent) -> Result<bool, api::Failure> {
        if matches!(event.action, ui::Action::Dismiss) && self.commit_hunks_open {
            self.commit_hunks_open = false;
            return Ok(true);
        }
        if !matches!(event.action, ui::Action::Click) {
            return Ok(false);
        }
        if event.node.starts_with("hunk-") {
            self.commit_hunks_open = false;
        }
        match event.node.as_str() {
            "commit-hunks-close" => self.commit_hunks_open = false,
            "commit-hunks" if !self.ui_busy() => {
                self.commit_hunks_open = !self.patch.hunks.is_empty()
            }
            "commit-expand" | "commit-collapse" if !self.ui_busy() => {
                self.commit_expand_all = event.node == "commit-expand";
                if let Some(handle) = self.trees.get("staging") {
                    let ids = self
                        .tree_paths
                        .iter()
                        .filter_map(|(id, path)| {
                            (id.starts_with("staging:") && matches!(path, TreePath::Directory(_)))
                                .then(|| id[8..].to_owned())
                        })
                        .collect();
                    tree::set_expanded(handle.clone(), ids, self.commit_expand_all)?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Match a group/directory/leaf to all observed files, including children not loaded by the UI.
    pub(super) fn commit_changes<'a>(&'a self, path: &TreePath) -> Vec<&'a Change> {
        let (path, directory) = match path {
            TreePath::Directory(path) => (path.as_str(), true),
            TreePath::Leaf(path) => (path.as_str(), false),
        };
        let (group, relative) = path.split_once('/').unwrap_or((path, ""));
        self.changes
            .iter()
            .filter(|change| {
                let member = match group {
                    "conflicts" => change.conflict,
                    "untracked" => !change.conflict && change.worktree == '?',
                    "changes" => !change.conflict && change.worktree != '?',
                    _ => false,
                };
                member
                    && if directory {
                        relative.is_empty()
                            || change
                                .path
                                .strip_prefix(relative)
                                .is_some_and(|tail| tail.starts_with('/'))
                    } else {
                        change.path == relative
                    }
            })
            .collect()
    }
}
