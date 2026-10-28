//! Git supplies filename meanings and branch policy through public workspace presentation.
use super::*;
use plugin_protocol::presentation;
mod comparison;
mod menus;
pub(super) use comparison::BranchComparison;

/// Only immutable presentation metadata is retained here; native documents keep their own text/Undo.
#[derive(Default)]
pub(super) struct WorkspaceUi {
    /// One instance-owned public resource is shared by file colors and title entries.
    pub resource: Option<api::ResourceHandle>,
    /// Only this branch's action submenu is materialized, keeping large branch lists bounded.
    pub opened_branch: Option<String>,
    /// Successful checkout targets, most recent first; browsing history never writes this list.
    pub recent: Vec<String>,
    /// Explicit branch comparison retains frozen object identities and its paged file chooser.
    pub comparison: Option<BranchComparison>,
    /// Retired comparison queries cannot publish into a later chooser, including A→B→A.
    pub comparison_generation: u64,
}

impl State {
    /// Successful ref metadata writes follow only their exact selected history target. Old queued
    /// log replies lose their generation before the ordinary post-write refresh is scheduled.
    pub(super) fn follow_branch_metadata_change(
        &mut self,
        args: &[String],
    ) -> Result<(), api::Failure> {
        let change = match args {
            [git, flag, separator, old, new]
                if git == "branch" && flag == "-m" && separator == "--" =>
            {
                Some((old, Some(format!("refs/heads/{new}"))))
            }
            [git, flag, separator, old] if git == "branch" && flag == "-d" && separator == "--" => {
                Some((old, None))
            }
            _ => None,
        };
        if let Some((old, replacement)) = change {
            if self.filter.as_deref() == Some(format!("refs/heads/{old}").as_str()) {
                self.history_refresh = None;
                self.select_history_filter(replacement)?;
            }
        }
        Ok(())
    }
    /// Context actions from an always-available workspace entry reveal their existing review owner.
    /// Already visible history and states without a mounted workspace contribution need no request.
    pub(super) fn reveal_workspace_history(&mut self) -> Result<(), api::Failure> {
        if self.workspace_ui.resource.is_some()
            && !self.visible_panels.get("history").copied().unwrap_or(false)
        {
            self.editor(
                api::EditorOperation::SetPanelVisibility {
                    panel: "history".into(),
                    visible: true,
                },
                "visibility",
            )?;
        }
        Ok(())
    }
    /// Publish only on complete workbench scenes; log scroll/hover frames never rebuild branch menus.
    pub(super) fn publish_workspace(&mut self) -> Result<(), api::Failure> {
        let Some(resource) = self.workspace_ui.resource.clone() else {
            return Ok(());
        };
        let files = self
            .changes
            .iter()
            .map(|change| {
                let tone = if change.conflict || change.index == 'D' || change.worktree == 'D' {
                    tree::Tone::Negative
                } else if [change.index, change.worktree]
                    .iter()
                    .any(|status| matches!(status, 'M' | 'R' | 'C' | 'T'))
                {
                    tree::Tone::Informative
                } else {
                    tree::Tone::Positive
                };
                presentation::FileDecoration {
                    path: change.path.clone(),
                    tone,
                    tooltip: ui::LocalizedText::new(
                        format!(
                            "Index: {}; Worktree: {}{}",
                            status_name(change.index, false),
                            status_name(change.worktree, false),
                            if change.conflict { "; Conflict" } else { "" }
                        ),
                        format!(
                            "暂存区：{}；工作区：{}{}",
                            status_name(change.index, true),
                            status_name(change.worktree, true),
                            if change.conflict { "；冲突" } else { "" }
                        ),
                    ),
                }
            })
            .collect();
        // A filename quota failure must not prevent publication of a still-valid branch entry.
        let file_result = presentation::publish_files(resource.clone(), files);
        let current = if !self.current_ref.is_empty() {
            branch_name(&self.current_ref).to_owned()
        } else if let Some(head) = &self.head {
            format!("{} (detached)", &head[..head.len().min(8)])
        } else {
            self.label("Git（无提交）", "Git (no commits)").to_owned()
        };
        presentation::publish_entries(
            resource,
            vec![presentation::Entry {
                id: "branch".into(),
                label: ui::LocalizedText::new(&current, &current),
                tooltip: ui::LocalizedText::new(
                    format!("Current branch: {current}"),
                    format!("当前分支：{current}"),
                ),
                icon: ui::ToolIcon::new("icons/git.svg", "icons/git.svg"),
                order: 100,
                disabled: !self.repository_available(),
                busy: self.ui_busy(),
                menu: self.workspace_menu(),
                on_open: Some(self.workspace_command(
                    "refresh",
                    "",
                    "Refresh local state",
                    "刷新本地状态",
                )),
            }],
        )?;
        file_result
    }
    /// Workspace actions have captured typed arguments, even when no Git panel is visible.
    fn workspace_command(&self, action: &str, target: &str, en: &str, zh: &str) -> tree::Command {
        tree::Command {
            command: "workspace-action".into(),
            label: ui::LocalizedText::new(en, zh),
            arguments: serde_json::json!({"action":action,"target":target}),
            disabled: false,
            separator_before: false,
        }
    }
    /// An explicit branch-row click prepares its submenu; common actions reveal the relevant panel.
    pub(super) fn workspace_action(
        &mut self,
        arguments: &serde_json::Value,
    ) -> Result<(), api::Failure> {
        let target = arguments["target"].as_str().unwrap_or_default();
        match arguments["action"].as_str().unwrap_or_default() {
            "refresh" => self.request_refresh(),
            "branch-menu" if self.refs.iter().any(|reference| reference == target) => {
                self.workspace_ui.opened_branch = Some(target.into())
            }
            "commit" => self.editor(
                api::EditorOperation::SetPanelVisibility {
                    panel: "staging".into(),
                    visible: true,
                },
                "visibility",
            )?,
            _ => return Err(failure("工作区操作已失效 / Workspace action expired")),
        }
        Ok(())
    }
    /// Record only a successful actual checkout. Existing repository preferences preserve it privately.
    pub(super) fn record_checkout(&mut self, branch: &str) -> Result<(), api::Failure> {
        let reference = format!("refs/heads/{branch}");
        self.workspace_ui.recent.retain(|item| item != &reference);
        self.workspace_ui.recent.insert(0, reference);
        self.workspace_ui.recent.truncate(20);
        self.repository_persist()
    }
}

/// Display names never substitute for full refs in command arguments or resource ownership.
fn branch_name(reference: &str) -> &str {
    reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/remotes/"))
        .unwrap_or(reference)
}

/// Porcelain columns remain independent, including staged edits followed by worktree deletion.
fn status_name(status: char, chinese: bool) -> &'static str {
    match (status, chinese) {
        ('.', false) => "Unchanged",
        ('.', true) => "无变化",
        ('?', false) => "Untracked",
        ('?', true) => "未跟踪",
        ('A', false) => "Added",
        ('A', true) => "新增",
        ('D', false) => "Deleted",
        ('D', true) => "删除",
        ('R', false) => "Renamed",
        ('R', true) => "重命名",
        ('C', false) => "Copied",
        ('C', true) => "复制",
        ('U', false) => "Conflict",
        ('U', true) => "冲突",
        (_, false) => "Modified",
        (_, true) => "修改",
    }
}
